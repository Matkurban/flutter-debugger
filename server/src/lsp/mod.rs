pub mod colors;
pub mod dart_file;
pub mod dart_parser;
pub mod progress;
pub mod pubspec;
pub mod state;
pub mod test_runner;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use dashmap::{DashMap, DashSet};
use futures_util::stream::{self, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock, Semaphore};
use tower_lsp_server::jsonrpc::Result as RpcResult;
use tower_lsp_server::ls_types::{
    CodeAction, CodeActionKind, CodeActionOptions, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CodeActionResponse, CodeLens, CodeLensOptions, CodeLensParams,
    ColorInformation, ColorPresentation, ColorPresentationParams, ColorProviderCapability, Command,
    CompletionOptions, CompletionParams, CompletionResponse, DidChangeConfigurationParams,
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, DocumentColorParams, DocumentLink,
    DocumentLinkOptions, DocumentLinkParams, ExecuteCommandOptions, ExecuteCommandParams, Hover,
    HoverContents, HoverParams, HoverProviderCapability, InitializeParams, InitializeResult,
    InitializedParams, InlayHint, InlayHintParams, MarkupContent, MarkupKind, MessageType, OneOf,
    Position, Range, Registration, ServerCapabilities, ServerInfo, ShowDocumentParams,
    TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
    request::{CodeLensRefresh, Request, ShowDocument},
};
use tower_lsp_server::{Client, LanguageServer, LspService, Server};

use crate::cache::{DiskCache, FlutterDevice, is_fresh, now_unix};
use crate::daemon::FlutterDaemonManager;
use crate::debug_config::ensure_zed_debug_config;
use crate::devtools::resolve_or_start_devtools_url;
use crate::ipc::{
    IpcRequest, ServiceOverlay, list_active_sessions, read_session_record, send_ipc_request,
};
use crate::lsp::colors::{build_color_presentations, find_dart_colors};
use crate::lsp::dart_file::{
    DartFileRole, build_dart_code_lenses, find_counterpart_test_or_impl,
    should_skip_auto_hot_reload,
};
use crate::lsp::dart_parser::build_closing_label_inlay_hints;
use crate::lsp::progress::ProgressHandle;
use crate::lsp::pubspec::{
    ChangelogEntry, DependencyEvaluation, DependencySection, DependencySource, IgnoreRule,
    PackageInfo, PubPackageCache, PubRegistryClient, VersionConstraint, add_ignore_rule,
    build_asset_hover_markdown, build_hosted_hover_markdown, build_ignored_fallback_hover_markdown,
    build_local_path_hover_markdown, build_pubspec_code_actions, build_pubspec_code_lenses,
    build_pubspec_diagnostics, build_pubspec_document_links, build_pubspec_inlay_hints,
    build_sdk_or_git_hover_markdown, build_version_completion_items, completion_context_at,
    ensure_ignore_file_exists, load_effective_ignore_config, load_sibling_lockfile, parse_pubspec,
    remove_package_ignore_rules, resolve_all_ignore_files, resolve_primary_ignore_file,
};
use crate::lsp::state::{
    DocumentKind, OpenDocument, ServerConfig, classify_document_path, file_path_to_uri,
    uri_to_file_path,
};
use crate::lsp::test_runner::{TestFilter, TestRunStore, run_tests_streaming};
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
    "flutterDebugger.refreshPubVersions",
    "flutterDebugger.ignorePubUpdate",
    "flutterDebugger.unignorePubUpdate",
    "flutterDebugger.openPubIgnoreFile",
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
    "flutterDebugger.openNativeDevTools",
    "flutterDebugger.generateDebugConfig",
    "flutterDebugger.screenshot",
];

pub struct FlutterLspBackend {
    client: Client,
    docs: Arc<DashMap<Uri, OpenDocument>>,
    config: Arc<RwLock<ServerConfig>>,
    workspace_roots: Arc<RwLock<Vec<PathBuf>>>,
    cache: DiskCache,
    devices: Arc<RwLock<Vec<FlutterDevice>>>,
    daemon: FlutterDaemonManager,
    pub_registry: Arc<RwLock<PubRegistryClient>>,
    pub_cache: Arc<PubPackageCache>,
    pub_refreshing: Arc<DashSet<Uri>>,
    running_pub_commands: Arc<DashMap<PathBuf, String>>,
    in_flight_changelogs: Arc<DashMap<String, Arc<Mutex<()>>>>,
    saved_pubspec_fingerprints: Arc<DashMap<Uri, Vec<String>>>,
    pubspec_gen_counter: Arc<AtomicU64>,
    device_scan_lock: Arc<Mutex<()>>,
    reload_debounce: Arc<DashMap<PathBuf, u64>>,
    reload_seq: Arc<AtomicU64>,
    watcher_abort: Arc<Mutex<Option<tokio::task::AbortHandle>>>,
    test_results: TestRunStore,
    runner: CommandRunner,
}

impl FlutterLspBackend {
    pub fn new(client: Client) -> Self {
        let cache = DiskCache::default_location();
        let initial_devices = cache.load_devices().map(|c| c.devices).unwrap_or_default();
        let devices = Arc::new(RwLock::new(initial_devices));
        let daemon = FlutterDaemonManager::new(cache.clone(), Arc::clone(&devices));
        let default_cfg = ServerConfig::default();
        let pub_cache = Arc::new(PubPackageCache::new(cache.root_dir()));
        let pub_registry = PubRegistryClient::new(default_cfg.pubspec.request_timeout_secs)
            .expect("failed to initialize HTTP client for pub registry");

        Self {
            client,
            docs: Arc::new(DashMap::new()),
            config: Arc::new(RwLock::new(default_cfg)),
            workspace_roots: Arc::new(RwLock::new(Vec::new())),
            cache,
            devices,
            daemon,
            pub_registry: Arc::new(RwLock::new(pub_registry)),
            pub_cache,
            pub_refreshing: Arc::new(DashSet::new()),
            running_pub_commands: Arc::new(DashMap::new()),
            in_flight_changelogs: Arc::new(DashMap::new()),
            saved_pubspec_fingerprints: Arc::new(DashMap::new()),
            pubspec_gen_counter: Arc::new(AtomicU64::new(1)),
            device_scan_lock: Arc::new(Mutex::new(())),
            reload_debounce: Arc::new(DashMap::new()),
            reload_seq: Arc::new(AtomicU64::new(1)),
            watcher_abort: Arc::new(Mutex::new(None)),
            test_results: TestRunStore::new(),
            runner: CommandRunner::new(),
        }
    }

    async fn log_info(&self, msg: impl Into<String>) {
        let text = msg.into();
        tracing::info!("{text}");
        self.client.log_message(MessageType::INFO, text).await;
    }

    async fn log_warn(&self, msg: impl Into<String>) {
        let text = msg.into();
        tracing::warn!("{text}");
        self.client.log_message(MessageType::WARNING, text).await;
    }

    fn start_device_daemon_for_project(&self, root: PathBuf, custom_sdk: Option<String>) {
        let client_ref = self.client.clone();
        self.daemon.ensure_started(
            root,
            custom_sdk,
            Some(Arc::new(move |devices| {
                let names: Vec<String> = devices
                    .iter()
                    .map(|d| format!("{} ({})", d.name, d.id))
                    .collect();
                let summary = format!(
                    "Flutter daemon updated devices ({}): [{}]",
                    devices.len(),
                    names.join(", ")
                );
                tracing::info!("{summary}");
                let c = client_ref.clone();
                tokio::spawn(async move {
                    c.log_message(MessageType::INFO, summary).await;
                    let _ = c.send_request::<CodeLensRefresh>(()).await;
                });
            })),
        );
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

    async fn resolve_effective_sdk_for_pubspec(
        &self,
        path: Option<&Path>,
        parsed: &crate::lsp::pubspec::ParsedPubspec,
    ) -> Option<VersionConstraint> {
        if parsed.environment_sdk.is_some() {
            return parsed.environment_sdk.clone();
        }
        let parent = path?.parent()?;
        let sibling_path = parent.join("pubspec.yaml");
        if path == Some(sibling_path.as_path()) {
            return None;
        }
        if let Some(sibling_uri) = file_path_to_uri(&sibling_path)
            && let Some(doc) = self.docs.get(&sibling_uri)
            && let Some(sdk) = doc.pubspec.as_ref().and_then(|p| p.environment_sdk.clone())
        {
            return Some(sdk);
        }
        let raw = tokio::fs::read_to_string(&sibling_path).await.ok()?;
        parse_pubspec(&raw).environment_sdk
    }

    async fn analyze_pubspec_document(&self, uri: Uri, text: String, force_refresh: bool) {
        let Some(path) = uri_to_file_path(&uri) else {
            return;
        };
        let kind = classify_document_path(&path);
        if !matches!(kind, DocumentKind::Pubspec | DocumentKind::PubspecOverrides) {
            return;
        }

        let parsed = parse_pubspec(&text);
        let project = find_project_for_path(&path);
        let doc_gen = self.pubspec_gen_counter.fetch_add(1, Ordering::SeqCst) + 1;

        // Preserve existing evaluations/lockfile/ignore snapshot temporarily to avoid inlay hint flicker while refreshing.
        let (prev_locked, prev_evals, prev_ignore) = self
            .docs
            .get(&uri)
            .map(|d| {
                (
                    d.locked_versions.clone(),
                    d.evaluations.clone(),
                    d.ignore_config.clone(),
                )
            })
            .unwrap_or_default();

        self.docs.insert(
            uri.clone(),
            OpenDocument {
                kind,
                text,
                project: project.clone(),
                pubspec: Some(parsed.clone()),
                locked_versions: prev_locked,
                evaluations: prev_evals,
                ignore_config: prev_ignore,
                generation: doc_gen,
                dart: None,
            },
        );

        let cfg = self.config.read().await.pubspec.clone();
        let workspace_roots = self.workspace_roots.read().await.clone();
        let ignore_config = load_effective_ignore_config(&path, &workspace_roots, &cfg.ignore_packages);
        let project_root = project.as_ref().map(|p| p.root.as_path());

        if !parsed.is_valid_dart_project {
            if let Some(mut doc) = self.docs.get_mut(&uri)
                && doc.generation == doc_gen
            {
                doc.locked_versions.clear();
                doc.evaluations.clear();
                doc.ignore_config = ignore_config;
            }
            self.client.publish_diagnostics(uri, Vec::new(), None).await;
            let _ = self.client.send_request::<InlayHintRefreshRequest>(()).await;
            let _ = self.client.send_request::<CodeLensRefresh>(()).await;
            return;
        }

        let locked_versions = load_sibling_lockfile(&path).await;

        if !cfg.enabled {
            if self.docs.get(&uri).is_none_or(|d| d.generation != doc_gen) {
                return;
            }
            if let Some(mut doc) = self.docs.get_mut(&uri) {
                doc.locked_versions = locked_versions;
                doc.evaluations.clear();
                doc.ignore_config = ignore_config;
            }
            let diags = build_pubspec_diagnostics(&parsed, &[], project_root, &cfg);
            self.client.publish_diagnostics(uri, diags, None).await;
            let _ = self.client.send_request::<InlayHintRefreshRequest>(()).await;
            let _ = self.client.send_request::<CodeLensRefresh>(()).await;
            return;
        }

        let effective_sdk = self
            .resolve_effective_sdk_for_pubspec(Some(&path), &parsed)
            .await;
        let default_hosted = cfg.resolved_hosted_url();

        let eligible_deps: Vec<(crate::lsp::pubspec::ParsedDependency, String)> = parsed
            .dependencies
            .iter()
            .filter_map(|dep| {
                let section_enabled = match dep.section {
                    DependencySection::Dependencies => true,
                    DependencySection::DevDependencies => cfg.check_dev_dependencies,
                    DependencySection::DependencyOverrides => cfg.check_dependency_overrides,
                };
                if !section_enabled {
                    return None;
                }
                match &dep.source {
                    DependencySource::Hosted { url } => {
                        let hosted = url
                            .as_deref()
                            .unwrap_or(&default_hosted)
                            .trim_end_matches('/')
                            .to_string();
                        Some((dep.clone(), hosted))
                    }
                    _ => None,
                }
            })
            .collect();

        let mut unique_keys: Vec<(String, String)> = Vec::new();
        let mut seen_keys: HashSet<(String, String)> = HashSet::new();
        let mut refresh_invalidate_keys: Vec<(String, String)> = Vec::new();
        for (dep, hosted) in &eligible_deps {
            let key = (hosted.clone(), dep.name.clone());
            if seen_keys.insert(key.clone()) {
                if !ignore_config.is_all_ignored(&dep.name) {
                    refresh_invalidate_keys.push(key.clone());
                }
                unique_keys.push(key);
            }
        }

        if force_refresh {
            self.pub_refreshing.insert(uri.clone());
            let _ = self.client.send_request::<CodeLensRefresh>(()).await;
            self.pub_cache.invalidate(&refresh_invalidate_keys).await;
        }

        let mut info_map: HashMap<(String, String), PackageInfo> = HashMap::new();
        let mut to_fetch: Vec<(String, String)> = Vec::new();

        for (hosted, name) in unique_keys {
            let is_all_ignored = ignore_config.is_all_ignored(&name);
            if (!force_refresh || is_all_ignored)
                && let Some(cached) = self
                    .pub_cache
                    .get_package(&hosted, &name, cfg.cache_ttl_secs)
                    .await
            {
                info_map.insert((hosted, name), cached);
            } else if !is_all_ignored {
                to_fetch.push((hosted, name));
            }
        }

        if !to_fetch.is_empty() {
            let total = to_fetch.len();
            let progress = ProgressHandle::begin(
                self.client.clone(),
                "Pub Dependencies",
                Some(format!("Checking 0/{total} package(s)...")),
            )
            .await;

            let registry = self.pub_registry.read().await.clone();
            let pub_cache = Arc::clone(&self.pub_cache);
            let concurrency = cfg.max_concurrency.clamp(1, 32);
            let semaphore = Arc::new(Semaphore::new(concurrency));
            let completed = Arc::new(AtomicUsize::new(0));
            let progress_ref = &progress;

            let fetched: Vec<((String, String), Option<PackageInfo>)> = stream::iter(to_fetch)
                .map(|(hosted, name)| {
                    let registry = registry.clone();
                    let pub_cache = Arc::clone(&pub_cache);
                    let semaphore = Arc::clone(&semaphore);
                    let completed = Arc::clone(&completed);
                    async move {
                        let _permit = semaphore.acquire_owned().await.ok();
                        let res = registry.fetch_package_info(&hosted, &name).await;
                        let done = completed.fetch_add(1, Ordering::SeqCst) + 1;
                        let pct = ((done * 100) / total.max(1)) as u32;
                        progress_ref
                            .report_with_percentage(format!("{done}/{total} — {name}"), pct)
                            .await;
                        match res {
                            Ok(info) => {
                                pub_cache.put_package(info.clone()).await;
                                ((hosted, name), Some(info))
                            }
                            Err(err) => {
                                tracing::debug!(
                                    package = %name,
                                    hosted_url = %hosted,
                                    %err,
                                    "failed to fetch package metadata from pub registry"
                                );
                                ((hosted, name), None)
                            }
                        }
                    }
                })
                .buffer_unordered(concurrency)
                .collect()
                .await;

            let mut succeeded = 0usize;
            for (key, maybe_info) in fetched {
                if let Some(info) = maybe_info {
                    info_map.insert(key, info);
                    succeeded += 1;
                }
            }
            progress
                .end(Some(format!("Checked {succeeded}/{total} package(s)")))
                .await;
        }

        if force_refresh {
            self.pub_refreshing.remove(&uri);
        }

        // Abort applying stale results if the document was edited or closed during network I/O.
        if self.docs.get(&uri).is_none_or(|d| d.generation != doc_gen) {
            if force_refresh {
                let _ = self.client.send_request::<CodeLensRefresh>(()).await;
            }
            return;
        }

        let evaluations: Vec<DependencyEvaluation> = eligible_deps
            .into_iter()
            .filter_map(|(dep, hosted)| {
                let info = info_map.get(&(hosted, dep.name.clone()))?.clone();
                let locked = locked_versions.get(&dep.name).cloned();
                let pkg_ignore = ignore_config.for_package(&dep.name);
                Some(DependencyEvaluation::evaluate(
                    dep,
                    info,
                    locked,
                    effective_sdk.as_ref(),
                    &cfg,
                    pkg_ignore,
                ))
            })
            .collect();

        if let Some(mut doc) = self.docs.get_mut(&uri) {
            if doc.generation != doc_gen {
                return;
            }
            doc.locked_versions = locked_versions;
            doc.evaluations = evaluations.clone();
            doc.ignore_config = ignore_config;
        } else {
            return;
        }

        let diags = build_pubspec_diagnostics(&parsed, &evaluations, project_root, &cfg);
        self.client.publish_diagnostics(uri, diags, None).await;
        let _ = self.client.send_request::<InlayHintRefreshRequest>(()).await;
        let _ = self.client.send_request::<CodeLensRefresh>(()).await;
    }

    async fn reanalyze_open_pubspecs_in_dir(&self, dir: &Path) {
        let targets: Vec<(Uri, String)> = self
            .docs
            .iter()
            .filter_map(|entry| {
                let doc = entry.value();
                if !matches!(doc.kind, DocumentKind::Pubspec | DocumentKind::PubspecOverrides) {
                    return None;
                }
                let doc_path = uri_to_file_path(entry.key())?;
                if doc_path.parent() == Some(dir) {
                    Some((entry.key().clone(), doc.text.clone()))
                } else {
                    None
                }
            })
            .collect();

        for (uri, text) in targets {
            let this = self.clone_handle();
            tokio::spawn(async move {
                this.analyze_pubspec_document(uri, text, false).await;
            });
        }
    }

    async fn reanalyze_open_pubspecs_under(&self, root: &Path) {
        let canonical_root = std::fs::canonicalize(root).ok();
        let targets: Vec<(Uri, String)> = self
            .docs
            .iter()
            .filter_map(|entry| {
                let doc = entry.value();
                if !matches!(doc.kind, DocumentKind::Pubspec | DocumentKind::PubspecOverrides) {
                    return None;
                }
                let doc_path = uri_to_file_path(entry.key())?;
                let direct = doc_path.starts_with(root);
                let canonical = !direct
                    && canonical_root.as_ref().is_some_and(|cr| {
                        std::fs::canonicalize(&doc_path)
                            .ok()
                            .is_some_and(|cd| cd.starts_with(cr))
                    });
                if direct || canonical {
                    Some((entry.key().clone(), doc.text.clone()))
                } else {
                    None
                }
            })
            .collect();

        for (uri, text) in targets {
            let this = self.clone_handle();
            tokio::spawn(async move {
                this.analyze_pubspec_document(uri, text, false).await;
            });
        }
    }

    async fn get_or_fetch_changelog(
        &self,
        hosted_url: &str,
        package: &str,
        ttl_secs: u64,
    ) -> Option<Vec<ChangelogEntry>> {
        if let Some(cached) = self
            .pub_cache
            .get_changelog(hosted_url, package, ttl_secs)
            .await
        {
            return Some(cached.entries);
        }

        let key = PubPackageCache::cache_key(hosted_url, package);
        let lock = self
            .in_flight_changelogs
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;

        // Re-check cache in case a concurrent hover populated it while we waited.
        if let Some(cached) = self
            .pub_cache
            .get_changelog(hosted_url, package, ttl_secs)
            .await
        {
            return Some(cached.entries);
        }

        let registry = self.pub_registry.read().await.clone();
        match tokio::time::timeout(
            Duration::from_secs(8),
            registry.fetch_changelog(hosted_url, package),
        )
        .await
        {
            Ok(Ok(cl)) => {
                let entries = cl.entries.clone();
                self.pub_cache.put_changelog(hosted_url, package, cl).await;
                Some(entries)
            }
            Ok(Err(err)) => {
                tracing::debug!(package = %package, %err, "changelog fetch failed");
                None
            }
            Err(_) => {
                tracing::debug!(package = %package, "changelog fetch timed out");
                None
            }
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
                    let names: Vec<String> = parsed
                        .iter()
                        .map(|d| format!("{} ({})", d.name, d.id))
                        .collect();
                    self.log_info(format!(
                        "Device scan in `{}` found {} device(s): [{}]",
                        cwd.display(),
                        parsed.len(),
                        names.join(", ")
                    ))
                    .await;
                    self.cache.save_devices(&parsed);
                    *self.devices.write().await = parsed.clone();
                    progress
                        .end(Some(format!("Found {} device(s)", parsed.len())))
                        .await;
                    let _ = self.client.send_request::<CodeLensRefresh>(()).await;
                } else {
                    self.log_warn("Unable to parse `flutter devices --machine` JSON output")
                        .await;
                    progress
                        .end(Some("Unable to parse device list".to_string()))
                        .await;
                }
            }
            Ok(output) => {
                let summary =
                    extract_error_summary(&output.stderr, &output.stdout, output.exit_code);
                self.log_warn(format!("Device scan failed: {summary}"))
                    .await;
                progress.end(Some("Device scan failed".to_string())).await;
                if force {
                    self.report_command_failure("flutter devices", &summary, output.log_path)
                        .await;
                }
            }
            Err(err) => {
                self.log_warn(format!("Device scan error: {err}")).await;
                progress.end(Some("Device scan failed".to_string())).await;
                if force {
                    self.client
                        .show_message(MessageType::ERROR, format!("Flutter devices failed: {err}"))
                        .await;
                }
            }
        }
    }

    async fn execute_project_cli(
        &self,
        uri: &Uri,
        cmd: AllowedCommand,
        silent_if_busy: bool,
    ) {
        let Some(path) = uri_to_file_path(uri) else {
            return;
        };
        let Some(project) = find_project_for_path(&path) else {
            if !silent_if_busy {
                self.client
                    .show_message(
                        MessageType::WARNING,
                        "No pubspec.yaml found for this file.".to_string(),
                    )
                    .await;
            }
            return;
        };

        let is_pub_cmd = matches!(
            cmd,
            AllowedCommand::PubGet | AllowedCommand::PubUpgrade | AllowedCommand::PubOutdated
        );
        let title = format!("{} {}", project.kind.cli_name(), cmd.display_name());

        if is_pub_cmd {
            if let Some(existing) = self.running_pub_commands.get(&project.root) {
                let running = existing.clone();
                drop(existing);
                if !silent_if_busy {
                    self.client
                        .show_message(
                            MessageType::INFO,
                            format!("`{running}` is already running for this project."),
                        )
                        .await;
                }
                return;
            }
            self.running_pub_commands
                .insert(project.root.clone(), title.clone());
            let _ = self.client.send_request::<CodeLensRefresh>(()).await;
        }

        let progress = ProgressHandle::begin(
            self.client.clone(),
            title.clone(),
            Some(format!("Running in {}...", project.root.display())),
        )
        .await;

        let custom_sdk = self.config.read().await.flutter_sdk_path.clone();
        let res = self
            .runner
            .run_project_command(&project, &cmd, custom_sdk.as_deref())
            .await;

        if is_pub_cmd {
            self.running_pub_commands.remove(&project.root);
            let _ = self.client.send_request::<CodeLensRefresh>(()).await;
        }

        match res {
            Ok(output) => {
                if !output.stdout.trim().is_empty() {
                    self.log_info(format!("[{title}]\n{}", output.stdout.trim()))
                        .await;
                }
                if !output.stderr.trim().is_empty() {
                    self.log_warn(format!("[{title} stderr]\n{}", output.stderr.trim()))
                        .await;
                }

                if output.success() {
                    progress.end(Some("Completed".to_string())).await;
                    if matches!(cmd, AllowedCommand::PubGet | AllowedCommand::PubUpgrade) {
                        self.reanalyze_open_pubspecs_in_dir(&project.root).await;
                    }
                    if cmd.always_open_log()
                        && let Some(log_path) = output.log_path
                    {
                        self.open_local_file_in_editor(&log_path).await;
                    } else if !silent_if_busy {
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
                if !silent_if_busy {
                    self.client
                        .show_message(MessageType::WARNING, err.to_string())
                        .await;
                }
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

        let mut devices = self.devices.read().await.clone();
        if devices.len() <= 1
            && let Some(cached) = self.cache.load_devices()
        {
            devices = merge_unique_devices(&devices, &cached.devices);
            if !devices.is_empty() {
                *self.devices.write().await = devices.clone();
            }
        }

        if devices.len() <= 1 {
            self.log_info(format!(
                "cycle_selected_device: only {} device(s) in memory/cache; triggering live `flutter devices --machine` scan...",
                devices.len()
            ))
            .await;
            let prior = devices.clone();
            self.maybe_scan_devices_in_background(root.clone(), true)
                .await;
            let scanned = self.devices.read().await.clone();
            devices = if scanned.len() >= prior.len() {
                scanned
            } else {
                merge_unique_devices(&scanned, &prior)
            };
        }

        if devices.is_empty() {
            self.log_warn("cycle_selected_device: no Flutter devices found after live scan")
                .await;
            self.client
                .show_message(
                    MessageType::WARNING,
                    "No Flutter devices found. Connect a device or start an emulator, then refresh."
                        .to_string(),
                )
                .await;
            return;
        }

        if devices.len() == 1 {
            let only_device = &devices[0];
            self.cache.set_selected_device(&root, &only_device.id);
            let _ = self.client.send_request::<CodeLensRefresh>(()).await;
            let msg = format!(
                "Only 1 Flutter device available: {}. Start an emulator or connect another device to switch.",
                only_device.display_label()
            );
            self.log_info(&msg).await;
            self.client.show_message(MessageType::INFO, msg).await;
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
        let msg = format!(
            "Flutter target device set to: {} ({}/{})",
            next_device.display_label(),
            next_idx + 1,
            devices.len()
        );
        self.log_info(&msg).await;
        self.client.show_message(MessageType::INFO, msg).await;
    }

    async fn stop_active_session(&self, uri: &Uri) {
        let Some(root) = self.resolve_project_root_for_uri(uri) else {
            let _ = self.client.send_request::<CodeLensRefresh>(()).await;
            return;
        };

        self.log_info(format!("Stopping active Flutter session for `{}`", root.display()))
            .await;

        let ipc_res = send_ipc_request(&self.cache, &root, &IpcRequest::Stop).await;
        // Always refresh CodeLens immediately on all paths
        let _ = self.client.send_request::<CodeLensRefresh>(()).await;

        match ipc_res {
            Some(resp) if resp.ok => {
                self.log_info(format!("Stop session succeeded: {}", resp.message))
                    .await;
                self.client
                    .show_message(MessageType::INFO, resp.message)
                    .await;
                let client_delayed = self.client.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    let _ = client_delayed.send_request::<CodeLensRefresh>(()).await;
                });
            }
            Some(resp) => {
                self.log_warn(format!("Stop session returned warning: {}", resp.message))
                    .await;
                self.client
                    .show_message(MessageType::WARNING, resp.message)
                    .await;
            }
            None => {
                self.log_warn("Stop session: no active Flutter session found for project")
                    .await;
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

    async fn open_native_devtools_for_uri(&self, uri: &Uri, page: Option<&str>) {
        let root = self.resolve_project_root_for_uri(uri);
        match crate::ctl::launch_native_devtools_ui(root.as_deref(), page) {
            Ok(msg) => {
                self.client.show_message(MessageType::INFO, msg).await;
            }
            Err(err) => {
                self.client.show_message(MessageType::WARNING, err).await;
            }
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

        match ensure_zed_debug_config(&project) {
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
            false,
        )
        .await;
    }
}

impl LanguageServer for FlutterLspBackend {
    async fn initialize(&self, params: InitializeParams) -> RpcResult<InitializeResult> {
        if let Some(opts) = params.initialization_options {
            let timeout_before = self.config.read().await.pubspec.request_timeout_secs;
            if self.config.write().await.apply_json_patch(&opts) {
                let timeout_after = self.config.read().await.pubspec.request_timeout_secs;
                if timeout_after != timeout_before
                    && let Ok(new_client) = PubRegistryClient::new(timeout_after)
                {
                    *self.pub_registry.write().await = new_client;
                }
            }
        }

        let mut roots: Vec<PathBuf> = Vec::new();
        if let Some(folders) = params.workspace_folders {
            for folder in folders {
                if let Some(path) = uri_to_file_path(&folder.uri)
                    && !roots.contains(&path)
                {
                    roots.push(path);
                }
            }
        }
        #[allow(deprecated)]
        if roots.is_empty()
            && let Some(ref root_uri) = params.root_uri
            && let Some(path) = uri_to_file_path(root_uri)
        {
            roots.push(path);
        }
        if roots.is_empty()
            && let Ok(cwd) = std::env::current_dir()
        {
            roots.push(cwd);
        }
        tracing::info!(
            "flutter-debugger LSP initialize: workspace_roots={:?}",
            roots
        );
        *self.workspace_roots.write().await = roots;

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec!["^".to_string(), ":".to_string()]),
                    resolve_provider: Some(false),
                    ..Default::default()
                }),
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(false),
                }),
                document_link_provider: Some(DocumentLinkOptions {
                    resolve_provider: Some(false),
                    work_done_progress_options: Default::default(),
                }),
                inlay_hint_provider: Some(OneOf::Left(true)),
                color_provider: Some(ColorProviderCapability::Simple(true)),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        code_action_kinds: Some(vec![
                            CodeActionKind::QUICKFIX,
                            CodeActionKind::SOURCE,
                        ]),
                        ..Default::default()
                    },
                )),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: SUPPORTED_COMMANDS.iter().map(|s| (*s).to_string()).collect(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "flutter-debugger".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            ..Default::default()
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        let roots = self.workspace_roots.read().await.clone();
        let discovered = discover_flutter_projects_in_roots(&roots);
        let cached_devices = self.devices.read().await.clone();
        let device_names: Vec<String> = cached_devices
            .iter()
            .map(|d| format!("{} ({})", d.name, d.id))
            .collect();
        let proj_summary: Vec<String> = discovered
            .iter()
            .map(|p| format!("{} ({:?})", p.root.display(), p.kind))
            .collect();

        self.log_info(format!(
            "flutter-debugger v{} initialized | workspaces={} | projects=[{}] | cached_devices={} [{}]",
            env!("CARGO_PKG_VERSION"),
            roots.len(),
            proj_summary.join(", "),
            cached_devices.len(),
            device_names.join(", ")
        ))
        .await;

        // Dynamically register file watchers on `**/pubspec.lock` and `**/.zed/pubspec-ignore.json`
        // so external `pub get` runs or manual edits to ignore rules immediately refresh open pubspecs.
        let lock_watcher = Registration {
            id: "flutter-debugger-pubspec-files".to_string(),
            method: "workspace/didChangeWatchedFiles".to_string(),
            register_options: Some(json!({
                "watchers": [
                    { "globPattern": "**/pubspec.lock" },
                    { "globPattern": "**/.zed/pubspec-ignore.json" }
                ]
            })),
        };
        if let Err(err) = self.client.register_capability(vec![lock_watcher]).await {
            tracing::debug!("Dynamic pubspec file watcher registration skipped by client: {err}");
        }

        if let Some(flutter_proj) = discovered
            .into_iter()
            .find(|p| p.kind == ProjectKind::Flutter)
        {
            let root = flutter_proj.root;
            let custom_sdk = self.config.read().await.flutter_sdk_path.clone();
            self.log_info(format!(
                "Starting Flutter device daemon eagerly for `{}`",
                root.display()
            ))
            .await;
            self.start_device_daemon_for_project(root.clone(), custom_sdk);
            if cached_devices.len() <= 1 {
                let this = self.clone_handle();
                tokio::spawn(async move {
                    this.maybe_scan_devices_in_background(root, false).await;
                });
            }
        }

        let cache = self.cache.clone();
        let client = self.client.clone();
        let docs = Arc::clone(&self.docs);
        let handle = tokio::spawn(async move {
            let mut last_sig = compute_external_state_signature(&cache);
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if docs.is_empty() {
                    continue;
                }
                let cur_sig = compute_external_state_signature(&cache);
                if cur_sig != last_sig {
                    tracing::debug!(
                        "External Flutter session/device state changed ({} -> {}), refreshing CodeLens",
                        last_sig,
                        cur_sig
                    );
                    last_sig = cur_sig;
                    let _ = client.send_request::<CodeLensRefresh>(()).await;
                }
            }
        });
        *self.watcher_abort.lock().await = Some(handle.abort_handle());
    }

    async fn shutdown(&self) -> RpcResult<()> {
        self.log_info("flutter-debugger LSP shutting down and terminating flutter daemon")
            .await;
        if let Some(h) = self.watcher_abort.lock().await.take() {
            h.abort();
        }
        self.daemon.shutdown().await;
        Ok(())
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        let timeout_before = self.config.read().await.pubspec.request_timeout_secs;
        let changed = self.config.write().await.apply_json_patch(&params.settings);
        if changed {
            let timeout_after = self.config.read().await.pubspec.request_timeout_secs;
            if timeout_after != timeout_before
                && let Ok(new_client) = PubRegistryClient::new(timeout_after)
            {
                *self.pub_registry.write().await = new_client;
            }

            let open_pubspecs: Vec<(Uri, String)> = self
                .docs
                .iter()
                .filter_map(|entry| {
                    let doc = entry.value();
                    if matches!(doc.kind, DocumentKind::Pubspec | DocumentKind::PubspecOverrides) {
                        Some((entry.key().clone(), doc.text.clone()))
                    } else {
                        None
                    }
                })
                .collect();
            for (uri, text) in open_pubspecs {
                let this = self.clone_handle();
                tokio::spawn(async move {
                    this.analyze_pubspec_document(uri, text, false).await;
                });
            }

            let _ = self.client.send_request::<InlayHintRefreshRequest>(()).await;
            let _ = self.client.send_request::<CodeLensRefresh>(()).await;
        }
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let mut lock_dirs: Vec<PathBuf> = Vec::new();
        let mut ignore_roots: Vec<PathBuf> = Vec::new();
        for change in params.changes {
            let Some(path) = uri_to_file_path(&change.uri) else {
                continue;
            };
            let file_name = path.file_name().and_then(|n| n.to_str());
            if file_name == Some("pubspec.lock")
                && let Some(parent) = path.parent()
            {
                let dir = parent.to_path_buf();
                if !lock_dirs.contains(&dir) {
                    lock_dirs.push(dir);
                }
            } else if file_name == Some("pubspec-ignore.json")
                && let Some(zed_dir) = path.parent()
                && zed_dir.file_name().and_then(|n| n.to_str()) == Some(".zed")
                && let Some(root) = zed_dir.parent()
            {
                let root_buf = root.to_path_buf();
                if !ignore_roots.contains(&root_buf) {
                    ignore_roots.push(root_buf);
                }
            }
        }
        for dir in lock_dirs {
            self.reanalyze_open_pubspecs_in_dir(&dir).await;
        }
        for root in ignore_roots {
            self.reanalyze_open_pubspecs_under(&root).await;
        }
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;

        let Some(doc) = OpenDocument::parse_sync(&uri, &text) else {
            return;
        };

        let kind = doc.kind;
        let project = doc.project.clone();
        let main_line = doc.dart.as_ref().and_then(|d| d.main_line);
        let initial_fp = doc.pubspec.as_ref().map(|p| p.dependency_fingerprint());

        self.docs.insert(uri.clone(), doc);

        if matches!(kind, DocumentKind::Pubspec | DocumentKind::PubspecOverrides) {
            if let Some(fp) = initial_fp {
                self.saved_pubspec_fingerprints.insert(uri.clone(), fp);
            }
            let this = self.clone_handle();
            let uri_clone = uri.clone();
            tokio::spawn(async move {
                this.analyze_pubspec_document(uri_clone, text, false).await;
            });
        }

        let Some(project) = project else {
            return;
        };

        if let Some(line) = main_line {
            self.log_info(format!(
                "Opened `{}` with main() at line {} (project: `{}`)",
                uri.as_str(),
                line + 1,
                project.root.display()
            ))
            .await;
        }

        if project.kind == ProjectKind::Flutter {
            let root = project.root.clone();
            let was_started = self.daemon.is_started();
            let custom_sdk = self.config.read().await.flutter_sdk_path.clone();
            self.start_device_daemon_for_project(root.clone(), custom_sdk);
            let has_devices = !self.devices.read().await.is_empty();
            if !was_started && !has_devices {
                let this = self.clone_handle();
                tokio::spawn(async move {
                    this.maybe_scan_devices_in_background(root, false).await;
                });
            }
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let Some(change) = params.content_changes.into_iter().last() else {
            return;
        };
        let Some(mut doc) = OpenDocument::parse_sync(&uri, &change.text) else {
            return;
        };

        if matches!(doc.kind, DocumentKind::Pubspec | DocumentKind::PubspecOverrides) {
            let doc_gen = self.pubspec_gen_counter.fetch_add(1, Ordering::SeqCst) + 1;
            if let Some(existing) = self.docs.get(&uri) {
                doc.locked_versions = existing.locked_versions.clone();
                doc.evaluations = existing.evaluations.clone();
                doc.ignore_config = existing.ignore_config.clone();
            }
            doc.generation = doc_gen;
            let text_clone = doc.text.clone();
            self.docs.insert(uri.clone(), doc);

            // Debounce pubspec re-evaluation (350ms) so rapid typing stays smooth.
            let this = self.clone_handle();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(350)).await;
                let still_current = this
                    .docs
                    .get(&uri)
                    .is_some_and(|d| d.generation == doc_gen);
                if still_current {
                    this.analyze_pubspec_document(uri, text_clone, false).await;
                }
            });
        } else {
            self.docs.insert(uri, doc);
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;

        let is_pubspec = self
            .docs
            .get(&uri)
            .is_some_and(|d| matches!(d.kind, DocumentKind::Pubspec | DocumentKind::PubspecOverrides));
        if is_pubspec {
            let text = params
                .text
                .or_else(|| self.docs.get(&uri).map(|d| d.text.clone()));
            if let Some(text) = text {
                let new_fp = parse_pubspec(&text).dependency_fingerprint();
                let prev_fp = self.saved_pubspec_fingerprints.insert(uri.clone(), new_fp.clone());
                let deps_changed = prev_fp.is_some_and(|old| old != new_fp);

                let this = self.clone_handle();
                let uri_analyze = uri.clone();
                tokio::spawn(async move {
                    this.analyze_pubspec_document(uri_analyze, text, false).await;
                });

                let pubspec_cfg = self.config.read().await.pubspec.clone();
                if deps_changed && pubspec_cfg.enabled && pubspec_cfg.auto_pub_get_on_save {
                    let this = self.clone_handle();
                    let uri_cli = uri.clone();
                    tokio::spawn(async move {
                        this.execute_project_cli(&uri_cli, AllowedCommand::PubGet, true)
                            .await;
                    });
                }
            }
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
        self.saved_pubspec_fingerprints.remove(&uri);
        self.pub_refreshing.remove(&uri);
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn hover(&self, params: HoverParams) -> RpcResult<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        // Never interfere with the official Dart LSP on `.dart` files.
        let snapshot = self.docs.get(&uri).and_then(|doc| {
            if !matches!(doc.kind, DocumentKind::Pubspec | DocumentKind::PubspecOverrides) {
                return None;
            }
            Some((
                doc.project.as_ref().map(|p| p.root.clone()),
                doc.pubspec.clone()?,
                doc.evaluations.clone(),
                doc.locked_versions.clone(),
                doc.ignore_config.clone(),
            ))
        });
        let Some((project_root, parsed, evaluations, locked_versions, ignore_config)) = snapshot
        else {
            return Ok(None);
        };

        // 1. Asset / font entry hover (local filesystem info)
        if let Some(asset) = parsed
            .assets
            .iter()
            .find(|a| position_in_range(pos, a.range))
        {
            let md = build_asset_hover_markdown(asset, project_root.as_deref());
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: md,
                }),
                range: Some(asset.range),
            }));
        }

        let cfg = self.config.read().await.pubspec.clone();

        // 2. Hosted dependency hover (package metadata, pub points, versions & lazy changelog)
        if cfg.enabled
            && let Some(eval) = evaluations.iter().find(|e| {
                position_in_range(pos, e.dep.name_range)
                    || position_in_range(pos, e.dep.version_range)
            })
        {
            let changelog = self
                .get_or_fetch_changelog(
                    &eval.info.hosted_url,
                    &eval.dep.name,
                    cfg.changelog_cache_ttl_secs,
                )
                .await;
            let md = build_hosted_hover_markdown(eval, changelog.as_deref(), &cfg);
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: md,
                }),
                range: Some(union_range(eval.dep.name_range, eval.dep.version_range)),
            }));
        }

        // 3. Local `path:`, `sdk:`, `git:`, or `*`-ignored uncached hosted dependency hover
        if let Some(dep) = parsed.dependencies.iter().find(|d| {
            let path_hit = match &d.source {
                DependencySource::Path { range, .. } => position_in_range(pos, *range),
                _ => false,
            };
            position_in_range(pos, d.name_range)
                || position_in_range(pos, d.version_range)
                || path_hit
        }) {
            match &dep.source {
                DependencySource::Path { path, range } => {
                    let md = build_local_path_hover_markdown(dep, path, project_root.as_deref());
                    return Ok(Some(Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: md,
                        }),
                        range: Some(union_range(dep.name_range, *range)),
                    }));
                }
                DependencySource::Sdk(_) | DependencySource::Git { .. } => {
                    if let Some(md) = build_sdk_or_git_hover_markdown(dep) {
                        return Ok(Some(Hover {
                            contents: HoverContents::Markup(MarkupContent {
                                kind: MarkupKind::Markdown,
                                value: md,
                            }),
                            range: Some(dep.name_range),
                        }));
                    }
                }
                DependencySource::Hosted { url } => {
                    let pkg_ignore = ignore_config.for_package(&dep.name);
                    if cfg.enabled && !pkg_ignore.is_empty() {
                        let default_hosted = cfg.resolved_hosted_url();
                        let hosted = url.as_deref().unwrap_or(&default_hosted);
                        let md = build_ignored_fallback_hover_markdown(
                            dep,
                            &pkg_ignore,
                            locked_versions.get(&dep.name).map(String::as_str),
                            hosted,
                        );
                        return Ok(Some(Hover {
                            contents: HoverContents::Markup(MarkupContent {
                                kind: MarkupKind::Markdown,
                                value: md,
                            }),
                            range: Some(union_range(dep.name_range, dep.version_range)),
                        }));
                    }
                }
            }
        }

        Ok(None)
    }

    async fn completion(&self, params: CompletionParams) -> RpcResult<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;

        // Never interfere with the official Dart LSP on `.dart` files.
        let snapshot = self.docs.get(&uri).and_then(|doc| {
            if !matches!(doc.kind, DocumentKind::Pubspec | DocumentKind::PubspecOverrides) {
                return None;
            }
            Some((
                doc.text.clone(),
                doc.pubspec.clone()?,
                doc.locked_versions.clone(),
                doc.ignore_config.clone(),
            ))
        });
        let Some((text, parsed, locked_versions, ignore_config)) = snapshot else {
            return Ok(None);
        };

        let cfg = self.config.read().await.pubspec.clone();
        if !cfg.enabled {
            return Ok(None);
        }

        let Some(ctx) = completion_context_at(&text, &parsed, pos) else {
            return Ok(None);
        };

        let default_hosted = cfg.resolved_hosted_url();
        let hosted_url = ctx
            .hosted_url
            .as_deref()
            .unwrap_or(&default_hosted)
            .trim_end_matches('/')
            .to_string();

        let info = if let Some(cached) = self
            .pub_cache
            .get_package(&hosted_url, &ctx.package_name, cfg.cache_ttl_secs)
            .await
        {
            cached
        } else {
            let registry = self.pub_registry.read().await.clone();
            match tokio::time::timeout(
                Duration::from_secs(3),
                registry.fetch_package_info(&hosted_url, &ctx.package_name),
            )
            .await
            {
                Ok(Ok(fetched)) => {
                    self.pub_cache.put_package(fetched.clone()).await;
                    fetched
                }
                _ => return Ok(None),
            }
        };

        let doc_path = uri_to_file_path(&uri);
        let effective_sdk = self
            .resolve_effective_sdk_for_pubspec(doc_path.as_deref(), &parsed)
            .await;

        let pkg_ignore = ignore_config.for_package(&ctx.package_name);
        let items = build_version_completion_items(
            &ctx,
            &info,
            locked_versions.get(&ctx.package_name).map(String::as_str),
            effective_sdk.as_ref(),
            &cfg,
            &pkg_ignore,
        );

        if items.is_empty() {
            Ok(None)
        } else {
            Ok(Some(CompletionResponse::Array(items)))
        }
    }

    async fn document_link(&self, params: DocumentLinkParams) -> RpcResult<Option<Vec<DocumentLink>>> {
        let uri = params.text_document.uri;
        let snapshot = self.docs.get(&uri).and_then(|doc| {
            if !matches!(doc.kind, DocumentKind::Pubspec | DocumentKind::PubspecOverrides) {
                return None;
            }
            Some((
                doc.project.as_ref().map(|p| p.root.clone()),
                doc.pubspec.clone()?,
            ))
        });
        let Some((project_root, parsed)) = snapshot else {
            return Ok(None);
        };
        let cfg = self.config.read().await.pubspec.clone();
        let default_hosted = cfg.resolved_hosted_url();
        Ok(Some(build_pubspec_document_links(
            &parsed,
            project_root.as_deref(),
            &default_hosted,
            cfg.enabled,
        )))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> RpcResult<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let Some(doc) = self.docs.get(&uri) else {
            return Ok(None);
        };
        let kind = doc.kind;
        let has_project = doc.project.is_some();
        let dart = doc.dart.clone();
        let evaluations = doc.evaluations.clone();
        drop(doc);

        let cfg = self.config.read().await.clone();
        match kind {
            DocumentKind::Dart => {
                if !has_project || !cfg.closing_labels {
                    return Ok(None);
                }
                let Some(parsed) = dart else {
                    return Ok(None);
                };
                let hints = build_closing_label_inlay_hints(
                    &parsed.closing_labels,
                    cfg.closing_labels_min_lines,
                );
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
                let hints = build_pubspec_inlay_hints(&evaluations, params.range, &cfg.pubspec);
                Ok(Some(hints))
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
        let kind = doc.kind;
        let project = doc.project.clone();
        let pubspec = doc.pubspec.clone();
        let evaluations = doc.evaluations.clone();
        let dart = doc.dart.clone();
        drop(doc);

        match kind {
            DocumentKind::Pubspec | DocumentKind::PubspecOverrides => {
                let Some(parsed) = pubspec else {
                    return Ok(None);
                };
                let cfg = self.config.read().await.pubspec.clone();
                let project_kind = project.as_ref().map_or(ProjectKind::Dart, |p| p.kind);
                let is_refreshing = self.pub_refreshing.contains(&uri);
                let running_cmd = project
                    .as_ref()
                    .and_then(|p| self.running_pub_commands.get(&p.root).map(|r| r.clone()));
                let lenses = build_pubspec_code_lenses(
                    &uri,
                    &parsed,
                    project_kind,
                    &evaluations,
                    is_refreshing,
                    running_cmd.as_deref(),
                    &cfg,
                );
                Ok(Some(lenses))
            }
            DocumentKind::Dart => {
                let (Some(project), Some(parsed)) = (project, dart) else {
                    return Ok(None);
                };
                let devices = self.devices.read().await;
                let selected_label = self.selected_device_label_sync(&project, &devices);
                let active_session = read_session_record(&self.cache, &project.root);
                let test_results = self.test_results.get_file_results(&uri);
                Ok(Some(build_dart_code_lenses(
                    &uri,
                    &parsed,
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
        let kind = doc.kind;
        let project = doc.project.clone();
        let pubspec = doc.pubspec.clone();
        let evaluations = doc.evaluations.clone();
        let ignore_config = doc.ignore_config.clone();
        let dart = doc.dart.clone();
        drop(doc);

        let uri_str = uri.as_str().to_string();
        let cfg = self.config.read().await.clone();
        let mut actions: Vec<CodeActionOrCommand> = Vec::new();

        match kind {
            DocumentKind::Pubspec | DocumentKind::PubspecOverrides => {
                if cfg.pubspec.enabled
                    && let Some(parsed) = pubspec
                {
                    actions = build_pubspec_code_actions(
                        &uri,
                        &parsed,
                        &evaluations,
                        &ignore_config,
                        params.range,
                    );
                }
            }
            DocumentKind::Dart => {
                let Some(project) = project else {
                    return Ok(None);
                };
                let cursor_line = params.range.start.line;
                if let Some(parsed) = dart
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

        self.log_info(format!(
            "Executing LSP command `{}` on `{}`",
            params.command,
            uri.as_str()
        ))
        .await;

        match params.command.as_str() {
            "flutterDebugger.refreshPubVersions" => {
                if let Some(text) = self.docs.get(&uri).map(|d| d.text.clone()) {
                    self.analyze_pubspec_document(uri.clone(), text, true).await;
                }
            }
            "flutterDebugger.ignorePubUpdate" => {
                let package = params.arguments.get(1).and_then(Value::as_str);
                let rule_raw = params.arguments.get(2).and_then(Value::as_str);
                if let (Some(pkg), Some(rule_str), Some(doc_path)) =
                    (package, rule_raw, uri_to_file_path(&uri))
                    && let Some(rule) = IgnoreRule::parse(rule_str)
                {
                    let workspace_roots = self.workspace_roots.read().await.clone();
                    if let Some(ignore_file) =
                        resolve_primary_ignore_file(&doc_path, &workspace_roots)
                    {
                        let canonical = rule.to_canonical_string();
                        match add_ignore_rule(&ignore_file, pkg, rule) {
                            Ok(()) => {
                                if let Some(root) = ignore_file.parent().and_then(Path::parent) {
                                    self.reanalyze_open_pubspecs_under(root).await;
                                } else if let Some(parent) = doc_path.parent() {
                                    self.reanalyze_open_pubspecs_in_dir(parent).await;
                                }
                                self.client
                                    .show_message(
                                        MessageType::INFO,
                                        format!(
                                            "Ignored `{pkg}` ({canonical}) in .zed/pubspec-ignore.json"
                                        ),
                                    )
                                    .await;
                            }
                            Err(err) => {
                                self.client.show_message(MessageType::WARNING, err).await;
                            }
                        }
                    }
                }
            }
            "flutterDebugger.unignorePubUpdate" => {
                let package = params.arguments.get(1).and_then(Value::as_str);
                if let (Some(pkg), Some(doc_path)) = (package, uri_to_file_path(&uri)) {
                    let workspace_roots = self.workspace_roots.read().await.clone();
                    let all_files = resolve_all_ignore_files(&doc_path, &workspace_roots);
                    match remove_package_ignore_rules(&all_files, pkg) {
                        Ok(true) => {
                            if let Some(primary) = all_files.first()
                                && let Some(root) = primary.parent().and_then(Path::parent)
                            {
                                self.reanalyze_open_pubspecs_under(root).await;
                            } else if let Some(parent) = doc_path.parent() {
                                self.reanalyze_open_pubspecs_in_dir(parent).await;
                            }
                            self.client
                                .show_message(
                                    MessageType::INFO,
                                    format!("Cleared ignore rules for `{pkg}`."),
                                )
                                .await;
                        }
                        Ok(false) => {
                            self.client
                                .show_message(
                                    MessageType::INFO,
                                    format!(
                                        "`{pkg}` has no entry in .zed/pubspec-ignore.json (check `pubspec.ignorePackages` in Zed settings.json if still ignored)."
                                    ),
                                )
                                .await;
                        }
                        Err(err) => {
                            self.client.show_message(MessageType::WARNING, err).await;
                        }
                    }
                }
            }
            "flutterDebugger.openPubIgnoreFile" => {
                if let Some(doc_path) = uri_to_file_path(&uri) {
                    let workspace_roots = self.workspace_roots.read().await.clone();
                    if let Some(ignore_file) =
                        resolve_primary_ignore_file(&doc_path, &workspace_roots)
                    {
                        match ensure_ignore_file_exists(&ignore_file) {
                            Ok(()) => self.open_local_file_in_editor(&ignore_file).await,
                            Err(err) => {
                                self.client.show_message(MessageType::WARNING, err).await;
                            }
                        }
                    }
                }
            }
            "flutterDebugger.pubGet" => {
                self.execute_project_cli(&uri, AllowedCommand::PubGet, false)
                    .await;
            }
            "flutterDebugger.pubUpgrade" => {
                self.execute_project_cli(&uri, AllowedCommand::PubUpgrade, false)
                    .await;
            }
            "flutterDebugger.pubOutdated" => {
                self.execute_project_cli(&uri, AllowedCommand::PubOutdated, false)
                    .await;
            }
            "flutterDebugger.clean" => {
                self.execute_project_cli(&uri, AllowedCommand::Clean, false)
                    .await;
            }
            "flutterDebugger.doctor" => {
                self.execute_project_cli(&uri, AllowedCommand::Doctor, false)
                    .await;
            }
            "flutterDebugger.runTest" => {
                let filter_str = params
                    .arguments
                    .get(1)
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let filter_kind = params
                    .arguments
                    .get(2)
                    .and_then(Value::as_str)
                    .unwrap_or("plain");
                let filter = match filter_str {
                    Some(s) if !s.trim().is_empty() && filter_kind == "regex" => {
                        TestFilter::NameRegex(s)
                    }
                    Some(s) if !s.trim().is_empty() => TestFilter::PlainName(s),
                    _ => TestFilter::All,
                };
                if let Some(path) = uri_to_file_path(&uri)
                    && let Some(project) = find_project_for_path(&path)
                {
                    let rel = path
                        .strip_prefix(&project.root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned();
                    let (is_integration, parsed_tests, main_line, source_text) = self
                        .docs
                        .get(&uri)
                        .map(|doc| {
                            let (is_int, tests, mline) = doc
                                .dart
                                .as_ref()
                                .map(|d| {
                                    (
                                        d.role == DartFileRole::IntegrationTest,
                                        d.tests.clone(),
                                        d.main_line,
                                    )
                                })
                                .unwrap_or((false, Vec::new(), None));
                            (is_int, tests, mline, doc.text.clone())
                        })
                        .unwrap_or((false, Vec::new(), None, String::new()));

                    let device_id = if is_integration && project.kind == ProjectKind::Flutter {
                        self.cache.get_selected_device(&project.root)
                    } else {
                        None
                    };
                    let custom_sdk = self.config.read().await.flutter_sdk_path.clone();
                    let client = self.client.clone();
                    let store = self.test_results.clone();
                    let uri_clone = uri.clone();
                    let (run_id, progress_token) = self.test_results.allocate_run();
                    let task_token = progress_token.clone();

                    let task = tokio::spawn(async move {
                        run_tests_streaming(
                            client,
                            store,
                            uri_clone,
                            run_id,
                            task_token,
                            project,
                            rel,
                            filter,
                            device_id,
                            custom_sdk,
                            parsed_tests,
                            main_line,
                            source_text,
                        )
                        .await;
                    });
                    self.test_results
                        .register_active_run(
                            &uri,
                            run_id,
                            task.abort_handle(),
                            progress_token,
                            &self.client,
                        )
                        .await;
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
            "flutterDebugger.openNativeDevTools" => {
                let page = params
                    .arguments
                    .get(1)
                    .and_then(Value::as_str);
                self.open_native_devtools_for_uri(&uri, page).await;
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
            workspace_roots: Arc::clone(&self.workspace_roots),
            cache: self.cache.clone(),
            devices: Arc::clone(&self.devices),
            daemon: self.daemon.clone(),
            pub_registry: Arc::clone(&self.pub_registry),
            pub_cache: Arc::clone(&self.pub_cache),
            pub_refreshing: Arc::clone(&self.pub_refreshing),
            running_pub_commands: Arc::clone(&self.running_pub_commands),
            in_flight_changelogs: Arc::clone(&self.in_flight_changelogs),
            saved_pubspec_fingerprints: Arc::clone(&self.saved_pubspec_fingerprints),
            pubspec_gen_counter: Arc::clone(&self.pubspec_gen_counter),
            device_scan_lock: Arc::clone(&self.device_scan_lock),
            reload_debounce: Arc::clone(&self.reload_debounce),
            reload_seq: Arc::clone(&self.reload_seq),
            watcher_abort: Arc::clone(&self.watcher_abort),
            test_results: self.test_results.clone(),
            runner: self.runner.clone(),
        }
    }
}

fn position_in_range(pos: Position, range: Range) -> bool {
    let after_start = pos.line > range.start.line
        || (pos.line == range.start.line && pos.character >= range.start.character);
    let before_end = pos.line < range.end.line
        || (pos.line == range.end.line && pos.character <= range.end.character);
    after_start && before_end
}

fn union_range(a: Range, b: Range) -> Range {
    let start = if a.start.line < b.start.line
        || (a.start.line == b.start.line && a.start.character <= b.start.character)
    {
        a.start
    } else {
        b.start
    };
    let end = if a.end.line > b.end.line
        || (a.end.line == b.end.line && a.end.character >= b.end.character)
    {
        a.end
    } else {
        b.end
    };
    Range::new(start, end)
}

fn merge_unique_devices(primary: &[FlutterDevice], secondary: &[FlutterDevice]) -> Vec<FlutterDevice> {
    let mut map: HashMap<String, FlutterDevice> = HashMap::new();
    for d in secondary {
        map.insert(d.id.clone(), d.clone());
    }
    for d in primary {
        map.insert(d.id.clone(), d.clone());
    }
    let mut out: Vec<FlutterDevice> = map.into_values().collect();
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    out
}

fn discover_flutter_projects_in_roots(roots: &[PathBuf]) -> Vec<ProjectInfo> {
    let mut projects: Vec<ProjectInfo> = Vec::new();
    let mut seen_roots: Vec<PathBuf> = Vec::new();

    let mut check_dir = |dir: &Path| {
        if let Some(proj) = find_project_for_path(dir)
            && !seen_roots.contains(&proj.root)
        {
            seen_roots.push(proj.root.clone());
            projects.push(proj);
        }
    };

    for root in roots {
        check_dir(root);
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.') || name_str == "build" || name_str == "target" {
                continue;
            }
            if path.join("pubspec.yaml").is_file() {
                check_dir(&path);
                continue;
            }
            // Check 1 deeper level for monorepos (`apps/<app>/pubspec.yaml`, `packages/<pkg>/pubspec.yaml`)
            if let Ok(sub_entries) = std::fs::read_dir(&path) {
                for sub in sub_entries.flatten() {
                    let sub_path = sub.path();
                    if sub_path.is_dir() && sub_path.join("pubspec.yaml").is_file() {
                        check_dir(&sub_path);
                    }
                }
            }
        }
    }

    projects
}

fn compute_external_state_signature(cache: &DiskCache) -> String {
    let selected_raw = std::fs::read_to_string(cache.root_dir().join("selected_devices.json"))
        .unwrap_or_default();
    let sessions_summary: Vec<String> = list_active_sessions(cache)
        .into_iter()
        .map(|s| {
            format!(
                "{}:{}:{}:{}",
                s.session_id,
                s.mode,
                s.vm_service_uri.unwrap_or_default(),
                s.devtools_uri.unwrap_or_default()
            )
        })
        .collect();
    format!("{selected_raw}|{}", sessions_summary.join(";"))
}

pub async fn run_lsp_server() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(FlutterLspBackend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_unique_devices_preserving_cached_entries() {
        let mem = vec![FlutterDevice {
            id: "macos".into(),
            name: "macOS".into(),
            target_platform: "darwin".into(),
            emulator: false,
            sdk: "macOS 15".into(),
        }];
        let cached = vec![
            FlutterDevice {
                id: "macos".into(),
                name: "macOS".into(),
                target_platform: "darwin".into(),
                emulator: false,
                sdk: "macOS 15".into(),
            },
            FlutterDevice {
                id: "chrome".into(),
                name: "Chrome".into(),
                target_platform: "web-javascript".into(),
                emulator: false,
                sdk: "Google Chrome".into(),
            },
        ];
        let merged = merge_unique_devices(&mem, &cached);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn discovers_flutter_projects_in_workspace_root_and_subdirectories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sub_app = dir.path().join("apps").join("mobile");
        std::fs::create_dir_all(&sub_app).expect("mkdir");
        std::fs::write(
            sub_app.join("pubspec.yaml"),
            "name: mobile\nflutter:\n  uses-material-design: true\n",
        )
        .expect("write pubspec");

        let found = discover_flutter_projects_in_roots(&[dir.path().to_path_buf()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].root, sub_app);
        assert_eq!(found[0].kind, ProjectKind::Flutter);
    }

    #[test]
    fn hover_range_helpers_handle_boundaries_and_unions() {
        let name = Range::new(Position::new(5, 2), Position::new(5, 5));
        let ver = Range::new(Position::new(5, 7), Position::new(5, 13));
        assert!(position_in_range(Position::new(5, 2), name));
        assert!(position_in_range(Position::new(5, 5), name));
        assert!(!position_in_range(Position::new(5, 6), name));

        let merged = union_range(ver, name);
        assert_eq!(merged.start, Position::new(5, 2));
        assert_eq!(merged.end, Position::new(5, 13));
        assert!(position_in_range(Position::new(5, 6), merged));
    }
}
