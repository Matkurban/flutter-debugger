pub mod colors;
pub mod dart_file;
pub mod dart_parser;
pub mod progress;
pub mod pubspec;
pub mod state;
pub mod test_runner;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock};
use tower_lsp_server::jsonrpc::Result as RpcResult;
use tower_lsp_server::ls_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CodeActionResponse, CodeLens, CodeLensOptions, CodeLensParams,
    ColorInformation, ColorPresentation, ColorPresentationParams, ColorProviderCapability, Command,
    DidChangeConfigurationParams, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, DocumentColorParams, DocumentLink,
    DocumentLinkOptions, DocumentLinkParams, ExecuteCommandOptions, ExecuteCommandParams,
    InitializeParams, InitializeResult, InitializedParams, InlayHint, InlayHintParams,
    MessageType, OneOf, Position, Range, ServerCapabilities, ServerInfo, ShowDocumentParams,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit, Uri, WorkspaceEdit,
    request::{CodeLensRefresh, Request, ShowDocument},
};
use tower_lsp_server::{Client, LanguageServer, LspService, Server};

use crate::cache::{
    CachedPackageVersion, DiskCache, FlutterDevice, PubVersionCache, is_fresh, now_unix,
};
use crate::daemon::FlutterDaemonManager;
use crate::debug_config::ensure_zed_debug_config;
use crate::devtools::resolve_or_start_devtools_url;
use crate::ipc::{IpcRequest, ServiceOverlay, read_session_record, send_ipc_request};
use crate::lsp::colors::{build_color_presentations, find_dart_colors};
use crate::lsp::dart_file::{
    DartFileRole, build_dart_code_lenses, find_counterpart_test_or_impl,
    should_skip_auto_hot_reload,
};
use crate::lsp::dart_parser::build_closing_label_inlay_hints;
use crate::lsp::progress::ProgressHandle;
use crate::lsp::pubspec::{
    build_document_links, build_inlay_hints, build_pubspec_asset_diagnostics,
    build_pubspec_code_lenses,
};
use crate::lsp::state::{
    DocumentKind, OpenDocument, ServerConfig, file_path_to_uri, uri_to_file_path,
};
use crate::lsp::test_runner::{TestRunStore, run_tests_streaming};
use crate::project::{ProjectInfo, ProjectKind, find_project_for_path};
use crate::runner::{AllowedCommand, CommandRunner, extract_error_summary};

/// Standard LSP `workspace/inlayHint/refresh` request.
enum InlayHintRefreshRequest {}

impl Request for InlayHintRefreshRequest {
    type Params = ();
    type Result = ();
    const METHOD: &'static str = "workspace/inlayHint/refresh";
}

const SUPPORTED_COMMANDS: &[&str] = &[
    "flutterDebugger.pubGet",
    "flutterDebugger.pubUpgrade",
    "flutterDebugger.pubOutdated",
    "flutterDebugger.clean",
    "flutterDebugger.doctor",
    "flutterDebugger.runTest",
    "flutterDebugger.stopApp",
    "flutterDebugger.toggleOverlay",
    "flutterDebugger.goToCounterpart",
    "flutterDebugger.refreshDevices",
    "flutterDebugger.cycleDevice",
    "flutterDebugger.hotReload",
    "flutterDebugger.hotRestart",
    "flutterDebugger.openDevTools",
    "flutterDebugger.generateDebugConfig",
    "flutterDebugger.screenshot",
];

pub struct FlutterLspBackend {
    client: Client,
    docs: Arc<DashMap<Uri, OpenDocument>>,
    config: Arc<RwLock<ServerConfig>>,
    cache: DiskCache,
    devices: Arc<RwLock<Vec<FlutterDevice>>>,
    daemon: FlutterDaemonManager,
    pub_versions: Arc<RwLock<PubVersionCache>>,
    device_scan_lock: Arc<Mutex<()>>,
    pub_scan_lock: Arc<Mutex<()>>,
    reload_debounce: Arc<DashMap<PathBuf, u64>>,
    reload_seq: Arc<AtomicU64>,
    test_results: TestRunStore,
    runner: CommandRunner,
    http: reqwest::Client,
}

impl FlutterLspBackend {
    pub fn new(client: Client) -> Self {
        let cache = DiskCache::default_location();
        let initial_devices = cache.load_devices().map(|c| c.devices).unwrap_or_default();
        let initial_pub_versions = cache.load_pub_versions();
        let devices = Arc::new(RwLock::new(initial_devices));
        let daemon = FlutterDaemonManager::new(cache.clone(), Arc::clone(&devices));

        let http = reqwest::Client::builder()
            .user_agent("flutter-debugger-zed/0.1.0")
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        Self {
            client,
            docs: Arc::new(DashMap::new()),
            config: Arc::new(RwLock::new(ServerConfig::default())),
            cache,
            devices,
            daemon,
            pub_versions: Arc::new(RwLock::new(initial_pub_versions)),
            device_scan_lock: Arc::new(Mutex::new(())),
            pub_scan_lock: Arc::new(Mutex::new(())),
            reload_debounce: Arc::new(DashMap::new()),
            reload_seq: Arc::new(AtomicU64::new(1)),
            test_results: TestRunStore::new(),
            runner: CommandRunner::new(),
            http,
        }
    }

    fn selected_device_label_sync(
        &self,
        project: &ProjectInfo,
        devices: &[FlutterDevice],
    ) -> Option<String> {
        let selected_id = self.cache.get_selected_device(&project.root)?;
        if let Some(dev) = devices.iter().find(|d| d.id == selected_id) {
            Some(dev.display_label())
        } else {
            Some(selected_id)
        }
    }

    fn resolve_project_root_for_uri(&self, uri: &Uri) -> Option<PathBuf> {
        let path = uri_to_file_path(uri)?;
        find_project_for_path(&path).map(|p| p.root)
    }

    async fn publish_pubspec_diagnostics_if_needed(&self, uri: &Uri, doc: &OpenDocument) {
        if !matches!(doc.kind, DocumentKind::Pubspec | DocumentKind::PubspecOverrides) {
            return;
        }
        let (Some(parsed), Some(project)) = (&doc.pubspec, &doc.project) else {
            return;
        };
        let diags = build_pubspec_asset_diagnostics(parsed, &project.root);
        self.client
            .publish_diagnostics(uri.clone(), diags, None)
            .await;
    }

    async fn maybe_scan_devices_in_background(&self, cwd: PathBuf, force: bool) {
        let ttl = self.config.read().await.device_cache_ttl_secs;
        if !force
            && let Some(cached) = self.cache.load_devices()
            && is_fresh(cached.fetched_at_unix, ttl)
        {
            *self.devices.write().await = cached.devices;
            return;
        }

        let Ok(_guard) = self.device_scan_lock.try_lock() else {
            return;
        };

        let progress = ProgressHandle::begin(
            self.client.clone(),
            "Flutter Devices",
            Some("Scanning connected devices...".to_string()),
        )
        .await;

        let custom_sdk = self.config.read().await.flutter_sdk_path.clone();
        match self
            .runner
            .run_flutter_devices(&cwd, custom_sdk.as_deref())
            .await
        {
            Ok(output) if output.success() => {
                if let Ok(parsed) = serde_json::from_str::<Vec<FlutterDevice>>(&output.stdout) {
                    self.cache.save_devices(&parsed);
                    *self.devices.write().await = parsed.clone();
                    progress
                        .end(Some(format!("Found {} device(s)", parsed.len())))
                        .await;
                    let _ = self.client.send_request::<CodeLensRefresh>(()).await;
                } else {
                    progress
                        .end(Some("Unable to parse device list".to_string()))
                        .await;
                }
            }
            Ok(output) => {
                let summary =
                    extract_error_summary(&output.stderr, &output.stdout, output.exit_code);
                progress.end(Some("Device scan failed".to_string())).await;
                if force {
                    self.report_command_failure("flutter devices", &summary, output.log_path)
                        .await;
                }
            }
            Err(err) => {
                progress.end(Some("Device scan failed".to_string())).await;
                if force {
                    self.client
                        .show_message(MessageType::ERROR, format!("Flutter devices failed: {err}"))
                        .await;
                }
            }
        }
    }

    async fn maybe_fetch_pub_versions(&self, packages: Vec<String>, force: bool) {
        if packages.is_empty() || !self.config.read().await.enable_pub_version_hints {
            return;
        }

        let ttl = self.config.read().await.pub_cache_ttl_secs;
        let to_fetch: Vec<String> = {
            let guard = self.pub_versions.read().await;
            packages
                .into_iter()
                .filter(|name| {
                    force
                        || guard
                            .packages
                            .get(name)
                            .is_none_or(|entry| !is_fresh(entry.fetched_at_unix, ttl))
                })
                .collect()
        };

        if to_fetch.is_empty() {
            return;
        }

        let Ok(_guard) = self.pub_scan_lock.try_lock() else {
            return;
        };

        let total = to_fetch.len();
        let progress = ProgressHandle::begin(
            self.client.clone(),
            "Pub Versions",
            Some(format!("Checking {total} package(s) on pub.dev...")),
        )
        .await;

        let mut updated = 0usize;
        for (idx, pkg) in to_fetch.iter().enumerate() {
            progress
                .report(format!("[{}/{}] {}", idx + 1, total, pkg))
                .await;
            if let Some(latest) = fetch_pub_dev_latest(&self.http, pkg).await {
                let mut guard = self.pub_versions.write().await;
                guard.packages.insert(
                    pkg.clone(),
                    CachedPackageVersion {
                        latest_version: latest,
                        fetched_at_unix: now_unix(),
                    },
                );
                updated += 1;
            }
        }

        {
            let guard = self.pub_versions.read().await;
            self.cache.save_pub_versions(&guard);
        }

        progress
            .end(Some(format!("Updated {updated}/{total} package(s)")))
            .await;

        if updated > 0 {
            let _ = self.client.send_request::<InlayHintRefreshRequest>(()).await;
        }
    }

    async fn execute_project_cli(&self, uri: &Uri, cmd: AllowedCommand) {
        let Some(path) = uri_to_file_path(uri) else {
            return;
        };
        let Some(project) = find_project_for_path(&path) else {
            self.client
                .show_message(
                    MessageType::WARNING,
                    "No pubspec.yaml found for this file.".to_string(),
                )
                .await;
            return;
        };

        let title = format!("{} {}", project.kind.cli_name(), cmd.display_name());
        let progress = ProgressHandle::begin(
            self.client.clone(),
            title.clone(),
            Some(format!("Running in {}...", project.root.display())),
        )
        .await;

        let custom_sdk = self.config.read().await.flutter_sdk_path.clone();
        match self
            .runner
            .run_project_command(&project, &cmd, custom_sdk.as_deref())
            .await
        {
            Ok(output) => {
                if output.success() {
                    progress.end(Some("Completed".to_string())).await;
                    if cmd.always_open_log()
                        && let Some(log_path) = output.log_path
                    {
                        self.open_local_file_in_editor(&log_path).await;
                    } else {
                        self.client
                            .show_message(MessageType::INFO, format!("{title} succeeded."))
                            .await;
                    }
                } else {
                    let summary =
                        extract_error_summary(&output.stderr, &output.stdout, output.exit_code);
                    progress.end(Some("Failed".to_string())).await;
                    self.report_command_failure(&title, &summary, output.log_path)
                        .await;
                }
            }
            Err(err) => {
                progress.end(Some("Blocked".to_string())).await;
                self.client
                    .show_message(MessageType::WARNING, err.to_string())
                    .await;
            }
        }
    }

    async fn report_command_failure(
        &self,
        title: &str,
        summary: &str,
        log_path: Option<PathBuf>,
    ) {
        if let Some(path) = log_path {
            self.open_local_file_in_editor(&path).await;
        }
        self.client
            .show_message(MessageType::ERROR, format!("{title} failed: {summary}"))
            .await;
    }

    async fn open_local_file_in_editor(&self, path: &Path) {
        if let Some(uri) = file_path_to_uri(path) {
            let _ = self
                .client
                .send_request::<ShowDocument>(ShowDocumentParams {
                    uri,
                    external: Some(false),
                    take_focus: Some(true),
                    selection: None,
                })
                .await;
        }
    }

    async fn cycle_selected_device(&self, uri: &Uri) {
        let Some(root) = self.resolve_project_root_for_uri(uri) else {
            return;
        };

        let devices = self.devices.read().await.clone();
        if devices.is_empty() {
            self.maybe_scan_devices_in_background(root.clone(), true)
                .await;
        }

        let devices = self.devices.read().await.clone();
        if devices.is_empty() {
            self.client
                .show_message(
                    MessageType::WARNING,
                    "No Flutter devices found. Connect a device or start an emulator, then refresh."
                        .to_string(),
                )
                .await;
            return;
        }

        let current = self.cache.get_selected_device(&root);
        let next_idx = match current
            .as_deref()
            .and_then(|id| devices.iter().position(|d| d.id == id))
        {
            Some(idx) => (idx + 1) % devices.len(),
            None => 0,
        };

        let next_device = &devices[next_idx];
        self.cache.set_selected_device(&root, &next_device.id);

        let _ = self.client.send_request::<CodeLensRefresh>(()).await;
        self.client
            .show_message(
                MessageType::INFO,
                format!(
                    "Flutter target device set to: {}",
                    next_device.display_label()
                ),
            )
            .await;
    }

    async fn stop_active_session(&self, uri: &Uri) {
        let Some(root) = self.resolve_project_root_for_uri(uri) else {
            return;
        };

        match send_ipc_request(&self.cache, &root, &IpcRequest::Stop).await {
            Some(resp) if resp.ok => {
                let _ = self.client.send_request::<CodeLensRefresh>(()).await;
                self.client
                    .show_message(MessageType::INFO, resp.message)
                    .await;
            }
            Some(resp) => {
                self.client
                    .show_message(MessageType::WARNING, resp.message)
                    .await;
            }
            None => {
                self.client
                    .show_message(
                        MessageType::WARNING,
                        "No active Flutter session found for this project.".to_string(),
                    )
                    .await;
            }
        }
    }

    async fn toggle_overlay_for_uri(&self, uri: &Uri, overlay: ServiceOverlay) {
        let Some(root) = self.resolve_project_root_for_uri(uri) else {
            return;
        };

        match send_ipc_request(&self.cache, &root, &IpcRequest::ToggleOverlay { overlay }).await {
            Some(resp) if resp.ok => {
                self.client
                    .show_message(MessageType::INFO, resp.message)
                    .await;
            }
            Some(resp) => {
                self.client
                    .show_message(MessageType::WARNING, resp.message)
                    .await;
            }
            None => {
                self.client
                    .show_message(
                        MessageType::WARNING,
                        "No active Flutter session found for this project.".to_string(),
                    )
                    .await;
            }
        }
    }

    async fn trigger_reload_or_restart(&self, uri: &Uri, full_restart: bool) {
        let Some(root) = self.resolve_project_root_for_uri(uri) else {
            return;
        };

        let req = if full_restart {
            IpcRequest::HotRestart
        } else {
            IpcRequest::HotReload
        };

        let title = if full_restart {
            "Hot Restart"
        } else {
            "Hot Reload"
        };
        let progress =
            ProgressHandle::begin(self.client.clone(), title, Some("Reloading...".to_string()))
                .await;

        match send_ipc_request(&self.cache, &root, &req).await {
            Some(resp) => {
                progress.end(Some(resp.message)).await;
            }
            None => {
                progress.end(Some("No active session".to_string())).await;
            }
        }
    }

    async fn open_devtools_for_uri(&self, uri: &Uri) {
        let Some(root) = self.resolve_project_root_for_uri(uri) else {
            return;
        };
        let custom_sdk = self.config.read().await.flutter_sdk_path.clone();

        let (dt, vm) = if let Some(session) = read_session_record(&self.cache, &root) {
            (session.devtools_uri, session.vm_service_uri)
        } else {
            (None, None)
        };

        let target = resolve_or_start_devtools_url(
            &self.cache,
            &root,
            dt.as_deref(),
            vm.as_deref(),
            None,
            custom_sdk.as_deref(),
        )
        .await;

        if let Some(url_str) = target
            && let Ok(target_uri) = url_str.parse::<Uri>()
        {
            let _ = self
                .client
                .send_request::<ShowDocument>(ShowDocumentParams {
                    uri: target_uri,
                    external: Some(true),
                    take_focus: Some(true),
                    selection: None,
                })
                .await;
        } else {
            self.client
                .show_message(
                    MessageType::WARNING,
                    "DevTools URI is not ready yet for this session.".to_string(),
                )
                .await;
        }
    }

    async fn go_to_counterpart_file(&self, uri: &Uri) {
        let Some(path) = uri_to_file_path(uri) else {
            return;
        };
        let Some(project) = find_project_for_path(&path) else {
            return;
        };
        if let Some((target, _)) = find_counterpart_test_or_impl(&path, &project.root) {
            self.open_local_file_in_editor(&target).await;
        }
    }

    async fn generate_debug_config(&self, uri: &Uri) {
        let Some(path) = uri_to_file_path(uri) else {
            return;
        };
        let Some(project) = find_project_for_path(&path) else {
            self.client
                .show_message(
                    MessageType::WARNING,
                    "No pubspec.yaml found for this file.".to_string(),
                )
                .await;
            return;
        };

        let selected_device = self.cache.get_selected_device(&project.root);
        match ensure_zed_debug_config(&project, selected_device.as_deref()) {
            Ok((config_path, created)) => {
                self.open_local_file_in_editor(&config_path).await;
                let msg = if created {
                    format!("Updated {}", config_path.display())
                } else {
                    format!("Opened existing {}", config_path.display())
                };
                self.client.show_message(MessageType::INFO, msg).await;
            }
            Err(err) => {
                self.client
                    .show_message(
                        MessageType::ERROR,
                        format!("Failed to generate .zed/debug.json: {err}"),
                    )
                    .await;
            }
        }
    }

    async fn save_screenshot(&self, uri: &Uri) {
        let Some(root) = self.resolve_project_root_for_uri(uri) else {
            return;
        };
        if let Some(resp) = send_ipc_request(
            &self.cache,
            &root,
            &IpcRequest::Screenshot { output_path: None },
        )
        .await
        {
            let ty = if resp.ok {
                MessageType::INFO
            } else {
                MessageType::WARNING
            };
            self.client.show_message(ty, resp.message).await;
            return;
        }

        let Some(project) = find_project_for_path(&root) else {
            return;
        };
        let device_id = self.cache.get_selected_device(&project.root);
        let output_path = project
            .root
            .join(format!("flutter_screenshot_{}.png", now_unix()));
        let output_str = output_path.to_string_lossy().into_owned();

        self.execute_project_cli(
            uri,
            AllowedCommand::Screenshot {
                device_id,
                output_path: output_str,
            },
        )
        .await;
    }
}

impl LanguageServer for FlutterLspBackend {
    async fn initialize(&self, params: InitializeParams) -> RpcResult<InitializeResult> {
        if let Some(opts) = params.initialization_options
            && let Ok(parsed) = serde_json::from_value::<ServerConfig>(opts)
        {
            *self.config.write().await = parsed;
        }

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(false),
                }),
                document_link_provider: Some(DocumentLinkOptions {
                    resolve_provider: Some(false),
                    work_done_progress_options: Default::default(),
                }),
                inlay_hint_provider: Some(OneOf::Left(true)),
                color_provider: Some(ColorProviderCapability::Simple(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: SUPPORTED_COMMANDS.iter().map(|s| (*s).to_string()).collect(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "flutter-debugger-server".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            ..Default::default()
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        tracing::info!("flutter-debugger-server LSP initialized");
    }

    async fn shutdown(&self) -> RpcResult<()> {
        self.daemon.shutdown().await;
        Ok(())
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        if let Ok(parsed) = serde_json::from_value::<ServerConfig>(params.settings) {
            *self.config.write().await = parsed;
        }
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;

        let Some(doc) = OpenDocument::parse_sync(&uri, &text) else {
            return;
        };

        self.publish_pubspec_diagnostics_if_needed(&uri, &doc).await;

        let project = doc.project.clone();
        let hosted_packages: Vec<String> = doc
            .pubspec
            .as_ref()
            .map(|p| {
                p.dependencies
                    .iter()
                    .filter(|d| d.is_hosted_pub_dev)
                    .map(|d| d.name.clone())
                    .collect()
            })
            .unwrap_or_default();

        self.docs.insert(uri, doc);

        let Some(project) = project else {
            return;
        };

        if project.kind == ProjectKind::Flutter {
            let root = project.root.clone();
            let custom_sdk = self.config.read().await.flutter_sdk_path.clone();
            let client_ref = self.client.clone();
            self.daemon.ensure_started(
                root.clone(),
                custom_sdk,
                Some(Arc::new(move |_devices| {
                    let c = client_ref.clone();
                    tokio::spawn(async move {
                        let _ = c.send_request::<CodeLensRefresh>(()).await;
                    });
                })),
            );
            let this = self.clone_handle();
            tokio::spawn(async move {
                this.maybe_scan_devices_in_background(root, false).await;
            });
        }

        if !hosted_packages.is_empty() && self.config.read().await.enable_pub_version_hints {
            let this = self.clone_handle();
            tokio::spawn(async move {
                this.maybe_fetch_pub_versions(hosted_packages, false).await;
            });
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(change) = params.content_changes.into_iter().last()
            && let Some(doc) = OpenDocument::parse_sync(&uri, &change.text)
        {
            self.publish_pubspec_diagnostics_if_needed(&uri, &doc).await;
            self.docs.insert(uri, doc);
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;

        if let Some(doc) = self.docs.get(&uri)
            && matches!(doc.kind, DocumentKind::Pubspec | DocumentKind::PubspecOverrides)
        {
            let doc_clone = doc.clone();
            drop(doc);
            self.publish_pubspec_diagnostics_if_needed(&uri, &doc_clone)
                .await;
            return;
        }

        let cfg = self.config.read().await.clone();
        if !cfg.auto_hot_reload_on_save {
            return;
        }

        let Some(path) = uri_to_file_path(&uri) else {
            return;
        };
        if should_skip_auto_hot_reload(&path) {
            return;
        }

        let project_root = self.docs.get(&uri).and_then(|doc| {
            if doc.kind == DocumentKind::Dart {
                doc.project.as_ref().map(|p| p.root.clone())
            } else {
                None
            }
        });

        let Some(root) = project_root else {
            return;
        };

        // Only schedule debounced reload when there is actually an active debug session
        let Some(session) = read_session_record(&self.cache, &root) else {
            return;
        };
        if session.mode != "debug" {
            return;
        }

        // Per-project debounce: coalesce rapid saves / format-on-save bursts into 1 Hot Reload
        let seq = self.reload_seq.fetch_add(1, Ordering::SeqCst);
        self.reload_debounce.insert(root.clone(), seq);

        let debounce_ms = cfg.hot_reload_debounce_ms.clamp(50, 2000);
        let debounce_map = Arc::clone(&self.reload_debounce);
        let cache = self.cache.clone();
        let client = self.client.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(debounce_ms)).await;
            let is_latest = debounce_map.get(&root).is_some_and(|v| *v == seq);
            if !is_latest {
                return;
            }

            let progress = ProgressHandle::begin(
                client.clone(),
                "Hot Reload",
                Some("Reloading...".to_string()),
            )
            .await;

            match send_ipc_request(&cache, &root, &IpcRequest::HotReload).await {
                Some(resp) => {
                    progress.end(Some(resp.message)).await;
                }
                None => {
                    progress.end(None).await;
                }
            }
        });
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.docs.remove(&uri);
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn document_link(&self, params: DocumentLinkParams) -> RpcResult<Option<Vec<DocumentLink>>> {
        let uri = params.text_document.uri;
        let Some(doc) = self.docs.get(&uri) else {
            return Ok(None);
        };
        let (Some(project), Some(parsed)) = (&doc.project, &doc.pubspec) else {
            return Ok(None);
        };
        let include_pub_dev = self.config.read().await.enable_pub_version_hints;
        Ok(Some(build_document_links(
            parsed,
            &project.root,
            include_pub_dev,
        )))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> RpcResult<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let Some(doc) = self.docs.get(&uri) else {
            return Ok(None);
        };
        if doc.project.is_none() {
            return Ok(None);
        }

        let cfg = self.config.read().await.clone();
        match doc.kind {
            DocumentKind::Dart => {
                if !cfg.closing_labels {
                    return Ok(None);
                }
                let Some(parsed) = &doc.dart else {
                    return Ok(None);
                };
                let hints = build_closing_label_inlay_hints(
                    &parsed.closing_labels,
                    cfg.closing_labels_min_lines,
                );
                // Filter to requested visible range
                let range = params.range;
                let filtered: Vec<InlayHint> = hints
                    .into_iter()
                    .filter(|h| {
                        h.position.line >= range.start.line && h.position.line <= range.end.line
                    })
                    .collect();
                Ok(Some(filtered))
            }
            DocumentKind::Pubspec | DocumentKind::PubspecOverrides => {
                if !cfg.enable_pub_version_hints {
                    return Ok(None);
                }
                let Some(parsed) = &doc.pubspec else {
                    return Ok(None);
                };
                let pub_cache = self.pub_versions.read().await;
                Ok(Some(build_inlay_hints(parsed, &pub_cache)))
            }
            DocumentKind::Ignored => Ok(None),
        }
    }

    async fn document_color(
        &self,
        params: DocumentColorParams,
    ) -> RpcResult<Vec<ColorInformation>> {
        if !self.config.read().await.color_previews {
            return Ok(Vec::new());
        }
        let uri = params.text_document.uri;
        let Some(doc) = self.docs.get(&uri) else {
            return Ok(Vec::new());
        };
        if doc.kind != DocumentKind::Dart || doc.project.is_none() {
            return Ok(Vec::new());
        }
        Ok(find_dart_colors(&doc.text))
    }

    async fn color_presentation(
        &self,
        params: ColorPresentationParams,
    ) -> RpcResult<Vec<ColorPresentation>> {
        Ok(build_color_presentations(params.color, params.range))
    }

    async fn code_lens(&self, params: CodeLensParams) -> RpcResult<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri;
        let Some(doc) = self.docs.get(&uri) else {
            return Ok(None);
        };
        let Some(project) = &doc.project else {
            return Ok(None);
        };

        match doc.kind {
            DocumentKind::Pubspec | DocumentKind::PubspecOverrides => {
                if self.config.read().await.enable_pub_version_hints {
                    Ok(Some(build_pubspec_code_lenses(&uri)))
                } else {
                    Ok(None)
                }
            }
            DocumentKind::Dart => {
                let Some(parsed) = &doc.dart else {
                    return Ok(None);
                };
                let devices = self.devices.read().await;
                let selected_label = self.selected_device_label_sync(project, &devices);
                let active_session = read_session_record(&self.cache, &project.root);
                let test_results = self.test_results.get_file_results(&uri);
                Ok(Some(build_dart_code_lenses(
                    &uri,
                    parsed,
                    project.kind,
                    selected_label.as_deref(),
                    active_session.as_ref(),
                    &test_results,
                )))
            }
            DocumentKind::Ignored => Ok(None),
        }
    }

    async fn code_action(&self, params: CodeActionParams) -> RpcResult<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let Some(doc) = self.docs.get(&uri) else {
            return Ok(None);
        };
        let Some(project) = &doc.project else {
            return Ok(None);
        };

        let uri_str = uri.as_str().to_string();
        let cfg = self.config.read().await.clone();
        let mut actions: Vec<CodeActionOrCommand> = Vec::new();

        match doc.kind {
            DocumentKind::Pubspec | DocumentKind::PubspecOverrides => {
                let cursor_line = params.range.start.line;
                if cfg.enable_pub_version_hints
                    && let Some(parsed) = &doc.pubspec
                {
                    let pub_cache = self.pub_versions.read().await;
                    if let Some(dep) = parsed
                        .dependencies
                        .iter()
                        .find(|d| d.line == cursor_line && d.is_hosted_pub_dev)
                        && let Some(cached) = pub_cache.packages.get(&dep.name)
                    {
                        let new_line = format!("  {}: ^{}", dep.name, cached.latest_version);
                        let mut changes = HashMap::new();
                        changes.insert(
                            uri.clone(),
                            vec![TextEdit {
                                range: Range {
                                    start: Position::new(dep.line, 0),
                                    end: Position::new(dep.line, dep.line_end_col),
                                },
                                new_text: new_line,
                            }],
                        );
                        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                            title: format!("Upgrade {} to ^{}", dep.name, cached.latest_version),
                            kind: Some(CodeActionKind::QUICKFIX),
                            edit: Some(WorkspaceEdit {
                                changes: Some(changes),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }));
                    }
                }
            }
            DocumentKind::Dart => {
                let cursor_line = params.range.start.line;
                if let Some(parsed) = &doc.dart
                    && parsed.role.is_runnable_test()
                    && let Some(entry) = parsed.tests.iter().find(|t| t.line == cursor_line)
                {
                    let title = format!("Run Test: {}", entry.local_name);
                    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                        title: title.clone(),
                        kind: Some(CodeActionKind::SOURCE),
                        command: Some(Command {
                            title,
                            command: "flutterDebugger.runTest".to_string(),
                            arguments: Some(vec![json!(uri_str), json!(entry.full_name)]),
                        }),
                        ..Default::default()
                    }));
                }

                if let Some(path) = uri_to_file_path(&uri)
                    && let Some((_, to_test)) = find_counterpart_test_or_impl(&path, &project.root)
                {
                    let title = if to_test {
                        "Dart: Go to Test File".to_string()
                    } else {
                        "Dart: Go to Implementation File".to_string()
                    };
                    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                        title: title.clone(),
                        kind: Some(CodeActionKind::SOURCE),
                        command: Some(Command {
                            title,
                            command: "flutterDebugger.goToCounterpart".to_string(),
                            arguments: Some(vec![json!(uri_str)]),
                        }),
                        ..Default::default()
                    }));
                }

                if cfg.expose_global_code_actions && project.kind == ProjectKind::Flutter {
                    for (title, cmd) in [
                        ("Flutter: Switch Target Device", "flutterDebugger.cycleDevice"),
                        ("Flutter: Generate .zed Config", "flutterDebugger.generateDebugConfig"),
                        ("Flutter: Clean Project", "flutterDebugger.clean"),
                        ("Flutter: Run Doctor", "flutterDebugger.doctor"),
                    ] {
                        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                            title: title.to_string(),
                            kind: Some(CodeActionKind::SOURCE),
                            command: Some(Command {
                                title: title.to_string(),
                                command: cmd.to_string(),
                                arguments: Some(vec![json!(uri_str)]),
                            }),
                            ..Default::default()
                        }));
                    }
                }
            }
            DocumentKind::Ignored => {}
        }

        if actions.is_empty() {
            Ok(None)
        } else {
            Ok(Some(actions))
        }
    }

    async fn execute_command(&self, params: ExecuteCommandParams) -> RpcResult<Option<Value>> {
        let first_uri = params
            .arguments
            .first()
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<Uri>().ok());

        let Some(uri) = first_uri else {
            return Ok(None);
        };

        match params.command.as_str() {
            "flutterDebugger.pubGet" => {
                self.execute_project_cli(&uri, AllowedCommand::PubGet).await;
            }
            "flutterDebugger.pubUpgrade" => {
                self.execute_project_cli(&uri, AllowedCommand::PubUpgrade)
                    .await;
            }
            "flutterDebugger.pubOutdated" => {
                self.execute_project_cli(&uri, AllowedCommand::PubOutdated)
                    .await;
            }
            "flutterDebugger.clean" => {
                self.execute_project_cli(&uri, AllowedCommand::Clean).await;
            }
            "flutterDebugger.doctor" => {
                self.execute_project_cli(&uri, AllowedCommand::Doctor).await;
            }
            "flutterDebugger.runTest" => {
                let test_name = params
                    .arguments
                    .get(1)
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                if let Some(path) = uri_to_file_path(&uri)
                    && let Some(project) = find_project_for_path(&path)
                {
                    let rel = path
                        .strip_prefix(&project.root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned();
                    let (is_integration, parsed_tests) = self
                        .docs
                        .get(&uri)
                        .and_then(|doc| {
                            doc.dart.as_ref().map(|d| {
                                (d.role == DartFileRole::IntegrationTest, d.tests.clone())
                            })
                        })
                        .unwrap_or((false, Vec::new()));

                    let device_id = if is_integration && project.kind == ProjectKind::Flutter {
                        self.cache.get_selected_device(&project.root)
                    } else {
                        None
                    };
                    let custom_sdk = self.config.read().await.flutter_sdk_path.clone();
                    let client = self.client.clone();
                    let store = self.test_results.clone();
                    let uri_clone = uri.clone();

                    tokio::spawn(async move {
                        run_tests_streaming(
                            client,
                            store,
                            uri_clone,
                            project,
                            rel,
                            test_name,
                            device_id,
                            custom_sdk,
                            parsed_tests,
                        )
                        .await;
                    });
                }
            }
            "flutterDebugger.stopApp" => {
                self.stop_active_session(&uri).await;
            }
            "flutterDebugger.toggleOverlay" => {
                if let Some(overlay) = params
                    .arguments
                    .get(1)
                    .and_then(Value::as_str)
                    .and_then(ServiceOverlay::parse)
                {
                    self.toggle_overlay_for_uri(&uri, overlay).await;
                }
            }
            "flutterDebugger.goToCounterpart" => {
                self.go_to_counterpart_file(&uri).await;
            }
            "flutterDebugger.refreshDevices" => {
                if let Some(root) = self.resolve_project_root_for_uri(&uri) {
                    self.maybe_scan_devices_in_background(root, true).await;
                }
            }
            "flutterDebugger.cycleDevice" => {
                self.cycle_selected_device(&uri).await;
            }
            "flutterDebugger.hotReload" => {
                self.trigger_reload_or_restart(&uri, false).await;
            }
            "flutterDebugger.hotRestart" => {
                self.trigger_reload_or_restart(&uri, true).await;
            }
            "flutterDebugger.openDevTools" => {
                self.open_devtools_for_uri(&uri).await;
            }
            "flutterDebugger.generateDebugConfig" => {
                self.generate_debug_config(&uri).await;
            }
            "flutterDebugger.screenshot" => {
                self.save_screenshot(&uri).await;
            }
            _ => {}
        }

        Ok(None)
    }
}

impl FlutterLspBackend {
    fn clone_handle(&self) -> Self {
        Self {
            client: self.client.clone(),
            docs: Arc::clone(&self.docs),
            config: Arc::clone(&self.config),
            cache: self.cache.clone(),
            devices: Arc::clone(&self.devices),
            daemon: self.daemon.clone(),
            pub_versions: Arc::clone(&self.pub_versions),
            device_scan_lock: Arc::clone(&self.device_scan_lock),
            pub_scan_lock: Arc::clone(&self.pub_scan_lock),
            reload_debounce: Arc::clone(&self.reload_debounce),
            reload_seq: Arc::clone(&self.reload_seq),
            test_results: self.test_results.clone(),
            runner: self.runner.clone(),
            http: self.http.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct PubDevPackageResponse {
    latest: PubDevLatest,
}

#[derive(Debug, Deserialize)]
struct PubDevLatest {
    version: String,
}

async fn fetch_pub_dev_latest(client: &reqwest::Client, package: &str) -> Option<String> {
    let url = format!("https://pub.dev/api/packages/{package}");
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.json::<PubDevPackageResponse>().await.ok()?;
    Some(body.latest.version)
}

pub async fn run_lsp_server() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(FlutterLspBackend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
