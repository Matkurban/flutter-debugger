pub mod dart_file;
pub mod progress;
pub mod pubspec;
pub mod state;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command as TokioCommand;
use tokio::sync::{Mutex, RwLock};
use tower_lsp_server::jsonrpc::Result as RpcResult;
use tower_lsp_server::ls_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CodeActionResponse, CodeLens, CodeLensOptions, CodeLensParams,
    Command, DidChangeConfigurationParams, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, DocumentLink, DocumentLinkOptions,
    DocumentLinkParams, ExecuteCommandOptions, ExecuteCommandParams, InitializeParams,
    InitializeResult, InitializedParams, InlayHint, InlayHintParams, MessageType, OneOf, Position,
    Range, ServerCapabilities, ServerInfo, ShowDocumentParams, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextEdit, Uri, WorkspaceEdit,
    request::{CodeLensRefresh, Request, ShowDocument},
};
use tower_lsp_server::{Client, LanguageServer, LspService, Server};

use crate::app_runner::{FlutterRunMode, ManagedAppManager};
use crate::cache::{
    CachedPackageVersion, DiskCache, FlutterDevice, PubVersionCache, is_fresh, now_unix,
};
use crate::ipc::{IpcRequest, read_session_record, send_ipc_request};
use crate::lsp::dart_file::build_dart_code_lenses;
use crate::lsp::progress::ProgressHandle;
use crate::lsp::pubspec::{build_document_links, build_inlay_hints, build_pubspec_code_lenses};
use crate::lsp::state::{
    DocumentKind, OpenDocument, ServerConfig, file_path_to_uri, uri_to_file_path,
};
use crate::project::{ProjectInfo, ProjectKind, find_project_for_path, resolve_sdk_executable};
use crate::runner::{
    AllowedCommand, CommandRunner, extract_error_summary, strip_ansi_codes, write_command_log,
};
use crate::wizard::{
    ProjectWizardConfig, build_wizard_code_lenses, create_wizard_file,
};

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
    "flutterDebugger.buildRunner",
    "flutterDebugger.runTest",
    "flutterDebugger.runMode",
    "flutterDebugger.stopApp",
    "flutterDebugger.refreshDevices",
    "flutterDebugger.cycleDevice",
    "flutterDebugger.refreshPubVersions",
    "flutterDebugger.hotReload",
    "flutterDebugger.hotRestart",
    "flutterDebugger.openDevTools",
    "flutterDebugger.openProjectWizard",
    "flutterDebugger.executeCreateProject",
    "flutterDebugger.cycleWizardTemplate",
];

pub struct FlutterLspBackend {
    client: Client,
    docs: Arc<DashMap<Uri, OpenDocument>>,
    config: Arc<RwLock<ServerConfig>>,
    cache: DiskCache,
    devices: Arc<RwLock<Vec<FlutterDevice>>>,
    pub_versions: Arc<RwLock<PubVersionCache>>,
    device_scan_lock: Arc<Mutex<()>>,
    pub_scan_lock: Arc<Mutex<()>>,
    runner: CommandRunner,
    app_manager: ManagedAppManager,
    http: reqwest::Client,
}

impl FlutterLspBackend {
    pub fn new(client: Client) -> Self {
        let cache = DiskCache::default_location();
        let initial_devices = cache
            .load_devices()
            .map(|c| c.devices)
            .unwrap_or_default();
        let initial_pub_versions = cache.load_pub_versions();

        let http = reqwest::Client::builder()
            .user_agent("flutter-debugger-zed/0.0.1")
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        Self {
            client,
            docs: Arc::new(DashMap::new()),
            config: Arc::new(RwLock::new(ServerConfig::default())),
            cache,
            devices: Arc::new(RwLock::new(initial_devices)),
            pub_versions: Arc::new(RwLock::new(initial_pub_versions)),
            device_scan_lock: Arc::new(Mutex::new(())),
            pub_scan_lock: Arc::new(Mutex::new(())),
            runner: CommandRunner::new(),
            app_manager: ManagedAppManager::new(),
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
        if packages.is_empty() {
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
        let Some(path) = uri_to_file_path(uri) else {
            return;
        };
        let Some(project) = find_project_for_path(&path) else {
            return;
        };

        let devices = self.devices.read().await.clone();
        if devices.is_empty() {
            self.maybe_scan_devices_in_background(project.root.clone(), true)
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

        let current = self.cache.get_selected_device(&project.root);
        let next_idx = match current
            .as_deref()
            .and_then(|id| devices.iter().position(|d| d.id == id))
        {
            Some(idx) => (idx + 1) % devices.len(),
            None => 0,
        };

        let next_device = &devices[next_idx];
        self.cache
            .set_selected_device(&project.root, &next_device.id);

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

    async fn run_flutter_in_mode(&self, uri: &Uri, mode: FlutterRunMode) {
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

        let target_file = path
            .strip_prefix(&project.root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        let selected_device = self.cache.get_selected_device(&project.root);
        let custom_sdk = self.config.read().await.flutter_sdk_path.clone();

        match self
            .app_manager
            .start_app(
                self.client.clone(),
                &project,
                &target_file,
                mode,
                selected_device.as_deref(),
                custom_sdk.as_deref(),
            )
            .await
        {
            Ok(log_path) => {
                self.open_local_file_in_editor(&log_path).await;
                let extra = if mode == FlutterRunMode::Profile {
                    " DevTools Performance view will open automatically once VM Service is ready."
                } else {
                    ""
                };
                self.client
                    .show_message(
                        MessageType::INFO,
                        format!("Launching Flutter in {} mode.{extra}", mode.as_str()),
                    )
                    .await;
            }
            Err(err) => {
                self.client.show_message(MessageType::WARNING, err).await;
            }
        }
    }

    async fn stop_managed_app(&self, uri: &Uri) {
        let Some(path) = uri_to_file_path(uri) else {
            return;
        };
        let Some(project) = find_project_for_path(&path) else {
            return;
        };

        match self.app_manager.stop_app(&project.root).await {
            Ok(msg) => {
                let _ = self.client.send_request::<CodeLensRefresh>(()).await;
                self.client.show_message(MessageType::INFO, msg).await;
            }
            Err(err) => {
                self.client.show_message(MessageType::WARNING, err).await;
            }
        }
    }

    async fn trigger_reload_or_restart(&self, uri: &Uri, full_restart: bool) {
        let Some(path) = uri_to_file_path(uri) else {
            return;
        };
        let Some(project) = find_project_for_path(&path) else {
            return;
        };

        // 1. Try managed runner session first
        if self.app_manager.snapshot(&project.root).await.is_some() {
            match self
                .app_manager
                .trigger_reload(&project.root, full_restart)
                .await
            {
                Ok(msg) => {
                    self.client.show_message(MessageType::INFO, msg).await;
                }
                Err(err) => {
                    self.client.show_message(MessageType::WARNING, err).await;
                }
            }
            return;
        }

        // 2. Fall back to DAP proxy IPC session
        let req = if full_restart {
            IpcRequest::HotRestart
        } else {
            IpcRequest::HotReload
        };

        match send_ipc_request(&self.cache, &project.root, &req).await {
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
                        "No active Flutter debug session found for this project.".to_string(),
                    )
                    .await;
            }
        }
    }

    async fn open_devtools_for_uri(&self, uri: &Uri) {
        let Some(path) = uri_to_file_path(uri) else {
            return;
        };
        let Some(project) = find_project_for_path(&path) else {
            return;
        };

        let target = if let Some(managed) = self.app_manager.snapshot(&project.root).await {
            managed.devtools_uri.or(managed.vm_service_uri)
        } else if let Some(session) = read_session_record(&self.cache, &project.root) {
            session.devtools_uri.or(session.vm_service_uri)
        } else {
            None
        };

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

    async fn open_project_wizard(&self, uri: &Uri) {
        let default_parent = uri_to_file_path(uri)
            .and_then(|p| find_project_for_path(&p).map(|proj| proj.root))
            .and_then(|root| root.parent().map(Path::to_path_buf))
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));

        match create_wizard_file(&default_parent) {
            Ok(wizard_path) => {
                self.open_local_file_in_editor(&wizard_path).await;
                self.client
                    .show_message(
                        MessageType::INFO,
                        "Edit project details and click `▷ Create Flutter Project` at line 1."
                            .to_string(),
                    )
                    .await;
            }
            Err(err) => {
                self.client
                    .show_message(
                        MessageType::ERROR,
                        format!("Failed to open project wizard: {err}"),
                    )
                    .await;
            }
        }
    }

    async fn cycle_wizard_template(&self, uri: &Uri) {
        let Some(path) = uri_to_file_path(uri) else {
            return;
        };
        let Ok(raw) = fs::read_to_string(&path) else {
            return;
        };
        let Ok(mut cfg) = serde_json::from_str::<ProjectWizardConfig>(&raw) else {
            return;
        };
        cfg.template = cfg.next_template().to_string();
        if let Ok(pretty) = serde_json::to_string_pretty(&cfg) {
            let _ = fs::write(&path, &pretty);
            if let Some(doc) = OpenDocument::parse_sync(uri, &pretty) {
                self.docs.insert(uri.clone(), doc);
            }
            let _ = self.client.send_request::<CodeLensRefresh>(()).await;
            self.client
                .show_message(
                    MessageType::INFO,
                    format!("Flutter project template set to `{}`.", cfg.template),
                )
                .await;
        }
    }

    async fn execute_create_project(&self, uri: &Uri) {
        let Some(path) = uri_to_file_path(uri) else {
            return;
        };
        let raw = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                self.client
                    .show_message(
                        MessageType::ERROR,
                        format!("Failed to read wizard file: {err}"),
                    )
                    .await;
                return;
            }
        };

        let cfg = match serde_json::from_str::<ProjectWizardConfig>(&raw) {
            Ok(c) => c,
            Err(err) => {
                self.client
                    .show_message(
                        MessageType::ERROR,
                        format!("Invalid wizard JSON: {err}"),
                    )
                    .await;
                return;
            }
        };

        let (target_dir, args) = match cfg.build_flutter_create_args() {
            Ok(res) => res,
            Err(err) => {
                self.client.show_message(MessageType::ERROR, err).await;
                return;
            }
        };

        let parent_dir = PathBuf::from(&cfg.output_dir);
        if let Err(err) = fs::create_dir_all(&parent_dir) {
            self.client
                .show_message(
                    MessageType::ERROR,
                    format!("Failed to prepare output directory: {err}"),
                )
                .await;
            return;
        }

        let progress = ProgressHandle::begin(
            self.client.clone(),
            "Flutter Create",
            Some(format!("Creating {} ({})...", cfg.name, cfg.template)),
        )
        .await;

        let custom_sdk = self.config.read().await.flutter_sdk_path.clone();
        let program =
            resolve_sdk_executable(&parent_dir, ProjectKind::Flutter, custom_sdk.as_deref());
        let command_line = format!("{program} {}", args.join(" "));

        let output_res = {
            #[cfg(windows)]
            {
                let lower = program.to_ascii_lowercase();
                if lower.ends_with(".bat") || lower.ends_with(".cmd") {
                    TokioCommand::new("cmd")
                        .arg("/C")
                        .arg(&program)
                        .args(&args)
                        .current_dir(&parent_dir)
                        .output()
                        .await
                } else {
                    TokioCommand::new(&program)
                        .args(&args)
                        .current_dir(&parent_dir)
                        .output()
                        .await
                }
            }
            #[cfg(not(windows))]
            {
                TokioCommand::new(&program)
                    .args(&args)
                    .current_dir(&parent_dir)
                    .output()
                    .await
            }
        };

        match output_res {
            Ok(out) => {
                let exit_code = out.status.code().unwrap_or(-1);
                let stdout = strip_ansi_codes(&String::from_utf8_lossy(&out.stdout));
                let stderr = strip_ansi_codes(&String::from_utf8_lossy(&out.stderr));

                if exit_code == 0 {
                    progress.end(Some("Project created".to_string())).await;
                    let pubspec = target_dir.join("pubspec.yaml");
                    if pubspec.is_file() {
                        self.open_local_file_in_editor(&pubspec).await;
                    }
                    self.client
                        .show_message(
                            MessageType::INFO,
                            format!("Created Flutter project at {}", target_dir.display()),
                        )
                        .await;
                } else {
                    let summary = extract_error_summary(&stderr, &stdout, exit_code);
                    let log_path = write_command_log(
                        "flutter-create",
                        &command_line,
                        &parent_dir,
                        exit_code,
                        &stderr,
                        &stdout,
                    )
                    .ok();
                    progress.end(Some("Creation failed".to_string())).await;
                    self.report_command_failure("flutter create", &summary, log_path)
                        .await;
                }
            }
            Err(err) => {
                progress.end(Some("Creation failed".to_string())).await;
                self.client
                    .show_message(
                        MessageType::ERROR,
                        format!("Failed to execute `{program}`: {err}"),
                    )
                    .await;
            }
        }
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

        // Synchronous parse & insert FIRST to prevent race conditions with immediate
        // `documentLink` / `codeLens` requests from Zed.
        let Some(doc) = OpenDocument::parse_sync(&uri, &text) else {
            return;
        };

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
            let this = self.clone_handle();
            tokio::spawn(async move {
                this.maybe_scan_devices_in_background(root, false).await;
            });
        }

        if !hosted_packages.is_empty() {
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
            self.docs.insert(uri, doc);
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let auto_reload = self.config.read().await.auto_hot_reload_on_save;
        if !auto_reload {
            return;
        }

        let uri = params.text_document.uri;
        let project_root = self.docs.get(&uri).and_then(|doc| {
            if matches!(doc.kind, DocumentKind::Dart { .. }) {
                doc.project.as_ref().map(|p| p.root.clone())
            } else {
                None
            }
        });

        let Some(root) = project_root else {
            return;
        };

        if let Some(managed) = self.app_manager.snapshot(&root).await {
            if managed.mode == FlutterRunMode::Debug {
                let _ = self.app_manager.trigger_reload(&root, false).await;
            }
            return;
        }

        if let Some(session) = read_session_record(&self.cache, &root)
            && session.mode == "debug"
        {
            let _ = send_ipc_request(&self.cache, &root, &IpcRequest::HotReload).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.docs.remove(&params.text_document.uri);
    }

    async fn document_link(&self, params: DocumentLinkParams) -> RpcResult<Option<Vec<DocumentLink>>> {
        let uri = params.text_document.uri;
        let Some(doc) = self.docs.get(&uri) else {
            return Ok(None);
        };
        if doc.project.is_none() {
            return Ok(None);
        }
        let Some(parsed) = &doc.pubspec else {
            return Ok(None);
        };
        Ok(Some(build_document_links(parsed)))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> RpcResult<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let Some(doc) = self.docs.get(&uri) else {
            return Ok(None);
        };
        if doc.project.is_none() {
            return Ok(None);
        }
        let Some(parsed) = &doc.pubspec else {
            return Ok(None);
        };

        let pub_cache = self.pub_versions.read().await;
        Ok(Some(build_inlay_hints(parsed, &pub_cache)))
    }

    async fn code_lens(&self, params: CodeLensParams) -> RpcResult<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri;
        let Some(doc) = self.docs.get(&uri) else {
            return Ok(None);
        };

        if doc.kind == DocumentKind::ProjectWizard {
            return Ok(Some(build_wizard_code_lenses(&uri, doc.wizard.as_ref())));
        }

        let Some(project) = &doc.project else {
            return Ok(None);
        };

        let devices = self.devices.read().await;
        let selected_label = self.selected_device_label_sync(project, &devices);

        match doc.kind {
            DocumentKind::Pubspec | DocumentKind::PubspecOverrides => Ok(Some(
                build_pubspec_code_lenses(&uri, project, selected_label.as_deref()),
            )),
            DocumentKind::Dart { .. } => {
                let Some(parsed) = &doc.dart else {
                    return Ok(None);
                };
                let dap_session = read_session_record(&self.cache, &project.root);
                let managed_app = self.app_manager.snapshot(&project.root).await;
                Ok(Some(build_dart_code_lenses(
                    &uri,
                    parsed,
                    project.kind,
                    selected_label.as_deref(),
                    dap_session.as_ref(),
                    managed_app.as_ref(),
                )))
            }
            DocumentKind::ProjectWizard | DocumentKind::Ignored => Ok(None),
        }
    }

    async fn code_action(&self, params: CodeActionParams) -> RpcResult<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let Some(doc) = self.docs.get(&uri) else {
            return Ok(None);
        };

        let uri_str = uri.as_str().to_string();

        if doc.kind == DocumentKind::ProjectWizard {
            return Ok(Some(vec![CodeActionOrCommand::Command(Command {
                title: "▷ Create Flutter Project".to_string(),
                command: "flutterDebugger.executeCreateProject".to_string(),
                arguments: Some(vec![json!(uri_str)]),
            })]));
        }

        let Some(project) = &doc.project else {
            return Ok(None);
        };

        let mut actions: Vec<CodeActionOrCommand> = Vec::new();

        match doc.kind {
            DocumentKind::Pubspec | DocumentKind::PubspecOverrides => {
                let cursor_line = params.range.start.line;
                if let Some(parsed) = &doc.pubspec {
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
                            title: format!("↑ Upgrade {} to ^{}", dep.name, cached.latest_version),
                            kind: Some(CodeActionKind::QUICKFIX),
                            edit: Some(WorkspaceEdit {
                                changes: Some(changes),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }));
                    }
                }

                actions.push(CodeActionOrCommand::Command(Command {
                    title: format!("↓ Run {} pub get", project.kind.cli_name()),
                    command: "flutterDebugger.pubGet".to_string(),
                    arguments: Some(vec![json!(uri_str)]),
                }));
                actions.push(CodeActionOrCommand::Command(Command {
                    title: "⊞ Flutter: New Project Wizard".to_string(),
                    command: "flutterDebugger.openProjectWizard".to_string(),
                    arguments: Some(vec![json!(uri_str)]),
                }));
            }
            DocumentKind::Dart { .. } => {
                let has_debug_managed = self
                    .app_manager
                    .snapshot(&project.root)
                    .await
                    .is_some_and(|m| m.mode == FlutterRunMode::Debug);
                let has_debug_dap = read_session_record(&self.cache, &project.root)
                    .is_some_and(|s| s.mode == "debug");

                if has_debug_managed || has_debug_dap {
                    actions.push(CodeActionOrCommand::Command(Command {
                        title: "⟳ Flutter: Hot Reload".to_string(),
                        command: "flutterDebugger.hotReload".to_string(),
                        arguments: Some(vec![json!(uri_str)]),
                    }));
                    actions.push(CodeActionOrCommand::Command(Command {
                        title: "↻ Flutter: Hot Restart".to_string(),
                        command: "flutterDebugger.hotRestart".to_string(),
                        arguments: Some(vec![json!(uri_str)]),
                    }));
                }

                if project.kind == ProjectKind::Flutter {
                    actions.push(CodeActionOrCommand::Command(Command {
                        title: "◎ Flutter: Switch Target Device".to_string(),
                        command: "flutterDebugger.cycleDevice".to_string(),
                        arguments: Some(vec![json!(uri_str)]),
                    }));
                    actions.push(CodeActionOrCommand::Command(Command {
                        title: "⊞ Flutter: New Project Wizard".to_string(),
                        command: "flutterDebugger.openProjectWizard".to_string(),
                        arguments: Some(vec![json!(uri_str)]),
                    }));
                }
            }
            DocumentKind::ProjectWizard | DocumentKind::Ignored => {}
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
            "flutterDebugger.buildRunner" => {
                self.execute_project_cli(&uri, AllowedCommand::BuildRunnerBuild)
                    .await;
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
                    self.execute_project_cli(
                        &uri,
                        AllowedCommand::TestFile {
                            relative_path: rel,
                            test_name,
                        },
                    )
                    .await;
                }
            }
            "flutterDebugger.runMode" => {
                let mode = params
                    .arguments
                    .get(1)
                    .and_then(Value::as_str)
                    .and_then(FlutterRunMode::parse)
                    .unwrap_or(FlutterRunMode::Debug);
                self.run_flutter_in_mode(&uri, mode).await;
            }
            "flutterDebugger.stopApp" => {
                self.stop_managed_app(&uri).await;
            }
            "flutterDebugger.refreshDevices" => {
                if let Some(path) = uri_to_file_path(&uri)
                    && let Some(project) = find_project_for_path(&path)
                {
                    self.maybe_scan_devices_in_background(project.root, true)
                        .await;
                }
            }
            "flutterDebugger.cycleDevice" => {
                self.cycle_selected_device(&uri).await;
            }
            "flutterDebugger.refreshPubVersions" => {
                let packages: Vec<String> = self
                    .docs
                    .get(&uri)
                    .and_then(|d| {
                        d.pubspec.as_ref().map(|p| {
                            p.dependencies
                                .iter()
                                .filter(|dep| dep.is_hosted_pub_dev)
                                .map(|dep| dep.name.clone())
                                .collect()
                        })
                    })
                    .unwrap_or_default();
                self.maybe_fetch_pub_versions(packages, true).await;
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
            "flutterDebugger.openProjectWizard" => {
                self.open_project_wizard(&uri).await;
            }
            "flutterDebugger.cycleWizardTemplate" => {
                self.cycle_wizard_template(&uri).await;
            }
            "flutterDebugger.executeCreateProject" => {
                self.execute_create_project(&uri).await;
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
            pub_versions: Arc::clone(&self.pub_versions),
            device_scan_lock: Arc::clone(&self.device_scan_lock),
            pub_scan_lock: Arc::clone(&self.pub_scan_lock),
            runner: self.runner.clone(),
            app_manager: self.app_manager.clone(),
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
