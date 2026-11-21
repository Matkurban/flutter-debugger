use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tower_lsp_server::Client;
use tower_lsp_server::ls_types::{
    CodeLens, Command as LspCommand, MessageType, Position, Range, ShowDocumentParams, Uri,
    request::{CodeLensRefresh, ShowDocument},
};

use crate::cache::{normalize_project_key, now_unix};
use crate::lsp::progress::ProgressHandle;
use crate::lsp::state::file_path_to_uri;
use crate::project::{ProjectInfo, ProjectKind, resolve_sdk_executable};
use crate::runner::strip_ansi_codes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlutterRunMode {
    Debug,
    Profile,
    Release,
}

impl FlutterRunMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "debug" => Some(Self::Debug),
            "profile" => Some(Self::Profile),
            "release" => Some(Self::Release),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Profile => "profile",
            Self::Release => "release",
        }
    }

    pub fn cli_flag(self) -> &'static str {
        match self {
            Self::Debug => "--debug",
            Self::Profile => "--profile",
            Self::Release => "--release",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugOverlayToggle {
    PerformanceOverlay,
    DebugPaint,
    RepaintRainbow,
    SlowAnimations,
    DebugBanner,
    PlatformOverride,
}

impl DebugOverlayToggle {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "performanceOverlay" => Some(Self::PerformanceOverlay),
            "debugPaint" => Some(Self::DebugPaint),
            "repaintRainbow" => Some(Self::RepaintRainbow),
            "slowAnimations" => Some(Self::SlowAnimations),
            "debugBanner" => Some(Self::DebugBanner),
            "platformOverride" => Some(Self::PlatformOverride),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::PerformanceOverlay => "Performance Overlay",
            Self::DebugPaint => "Debug Paint",
            Self::RepaintRainbow => "Repaint Rainbow",
            Self::SlowAnimations => "Slow Animations",
            Self::DebugBanner => "Debug Banner",
            Self::PlatformOverride => "Target Platform",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ManagedAppSnapshot {
    pub project_root: PathBuf,
    pub mode: FlutterRunMode,
    pub vm_service_uri: Option<String>,
    pub devtools_uri: Option<String>,
}

#[derive(Default)]
struct OverlayStates {
    performance_overlay: bool,
    debug_paint: bool,
    repaint_rainbow: bool,
    slow_animations: bool,
    debug_banner: bool,
    platform_index: usize,
}

struct ManagedAppInner {
    project_root: PathBuf,
    log_path: PathBuf,
    mode: FlutterRunMode,
    app_id: Option<String>,
    vm_service_uri: Option<String>,
    devtools_uri: Option<String>,
    overlays: OverlayStates,
}

struct ManagedAppHandle {
    state: Arc<RwLock<ManagedAppInner>>,
    stdin_tx: mpsc::Sender<String>,
    stop_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    next_rpc_id: Arc<AtomicI64>,
}

/// Manages active `flutter run --machine` sessions launched from `main()` CodeLens buttons
/// (`Debug`, `Profile`, `Release`) with reliable process termination and live log control bars.
#[derive(Clone, Default)]
pub struct ManagedAppManager {
    sessions: Arc<DashMap<String, ManagedAppHandle>>,
}

impl ManagedAppManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn snapshot(&self, project_root: &Path) -> Option<ManagedAppSnapshot> {
        let key = normalize_project_key(project_root);
        let state = self.sessions.get(&key).map(|entry| Arc::clone(&entry.state))?;
        let guard = state.read().await;
        Some(ManagedAppSnapshot {
            project_root: guard.project_root.clone(),
            mode: guard.mode,
            vm_service_uri: guard.vm_service_uri.clone(),
            devtools_uri: guard.devtools_uri.clone(),
        })
    }

    /// Finds the active managed app associated with a live `flutter-run-*.log` file.
    pub async fn snapshot_by_log_path(&self, log_path: &Path) -> Option<ManagedAppSnapshot> {
        let states: Vec<Arc<RwLock<ManagedAppInner>>> = self
            .sessions
            .iter()
            .map(|entry| Arc::clone(&entry.state))
            .collect();

        let canonical_target = fs::canonicalize(log_path).unwrap_or_else(|_| log_path.to_path_buf());
        for state in states {
            let guard = state.read().await;
            let canonical_entry =
                fs::canonicalize(&guard.log_path).unwrap_or_else(|_| guard.log_path.clone());
            if canonical_entry == canonical_target {
                return Some(ManagedAppSnapshot {
                    project_root: guard.project_root.clone(),
                    mode: guard.mode,
                    vm_service_uri: guard.vm_service_uri.clone(),
                    devtools_uri: guard.devtools_uri.clone(),
                });
            }
        }
        None
    }

    pub async fn start_app(
        &self,
        client: Client,
        project: &ProjectInfo,
        target_file: &str,
        mode: FlutterRunMode,
        device_id: Option<&str>,
        custom_sdk_path: Option<&str>,
    ) -> Result<PathBuf, String> {
        if project.kind != ProjectKind::Flutter {
            return Err("Debug/Profile/Release runner is only available for Flutter projects".into());
        }

        let key = normalize_project_key(&project.root);
        if self.sessions.contains_key(&key) {
            return Err(
                "A Flutter app is already running for this project. Stop it before launching another."
                    .into(),
            );
        }

        let program = resolve_sdk_executable(&project.root, ProjectKind::Flutter, custom_sdk_path);
        let mut args = vec![
            "run".to_string(),
            "--machine".to_string(),
            mode.cli_flag().to_string(),
            "-t".to_string(),
            target_file.to_string(),
        ];
        if let Some(dev) = device_id
            && !dev.trim().is_empty()
        {
            args.push("-d".to_string());
            args.push(dev.to_string());
        }

        let mut child = spawn_flutter_run(&program, &args, &project.root)
            .map_err(|err| format!("Failed to launch `{program}`: {err}"))?;

        let child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Failed to capture flutter run stdin".to_string())?;
        let child_stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to capture flutter run stdout".to_string())?;
        let child_stderr = child
            .stderr
            .take()
            .ok_or_else(|| "Failed to capture flutter run stderr".to_string())?;

        let log_dir = std::env::temp_dir().join("flutter-debugger-logs");
        let _ = fs::create_dir_all(&log_dir);
        let log_path = log_dir.join(format!("flutter-run-{}-{}.log", mode.as_str(), now_unix()));
        let header = format!(
            "Command: {program} {}\nWorking Directory: {}\nMode: {}\n\n",
            args.join(" "),
            project.root.display(),
            mode.as_str()
        );
        let _ = fs::write(&log_path, header);

        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(32);
        let (stop_tx, stop_rx) = oneshot::channel::<()>();

        let state = Arc::new(RwLock::new(ManagedAppInner {
            project_root: project.root.clone(),
            log_path: log_path.clone(),
            mode,
            app_id: None,
            vm_service_uri: None,
            devtools_uri: None,
            overlays: OverlayStates {
                debug_banner: true,
                ..Default::default()
            },
        }));

        self.sessions.insert(
            key.clone(),
            ManagedAppHandle {
                state: Arc::clone(&state),
                stdin_tx,
                stop_tx: Arc::new(Mutex::new(Some(stop_tx))),
                next_rpc_id: Arc::new(AtomicI64::new(1)),
            },
        );

        let _ = client.send_request::<CodeLensRefresh>(()).await;

        // Task 1: forward JSON-RPC commands to flutter run --machine stdin
        let stdin_task = tokio::spawn(async move {
            let mut writer = child_stdin;
            while let Some(line) = stdin_rx.recv().await {
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                let _ = writer.flush().await;
            }
        });

        // Task 2: append stderr to log file
        let err_log_path = log_path.clone();
        let stderr_task = tokio::spawn(async move {
            let mut reader = BufReader::new(child_stderr);
            let mut line = String::new();
            while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                append_log(&err_log_path, &format!("[stderr] {}", strip_ansi_codes(&line)));
                line.clear();
            }
        });

        // Task 3: supervise stdout events AND stop_rx for guaranteed process termination
        let sessions_ref = Arc::clone(&self.sessions);
        let project_root = project.root.clone();
        let custom_sdk_owned = custom_sdk_path.map(ToOwned::to_owned);
        let out_log_path = log_path.clone();
        let client_clone = client.clone();

        tokio::spawn(async move {
            let progress = ProgressHandle::begin(
                client_clone.clone(),
                format!("Flutter ({})", mode.as_str()),
                Some("Launching application...".to_string()),
            )
            .await;

            let mut reader = BufReader::new(child_stdout);
            let mut line = String::new();
            let mut started_notified = false;
            let mut devtools_launched = false;
            let mut user_stopped = false;

            tokio::pin!(stop_rx);

            loop {
                line.clear();
                tokio::select! {
                    biased;
                    _ = &mut stop_rx => {
                        user_stopped = true;
                        append_log(&out_log_path, "\n[flutter-debugger] Stop requested...\n");
                        let graceful = tokio::time::timeout(
                            Duration::from_millis(1500),
                            child.wait(),
                        )
                        .await;
                        if graceful.is_err() {
                            terminate_child_tree(&mut child).await;
                        }
                        break;
                    }
                    read_res = reader.read_line(&mut line) => {
                        match read_res {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {
                                let clean_line = strip_ansi_codes(&line);
                                if let Some(events) = parse_daemon_events(&clean_line) {
                                    for event in events {
                                        let event_name = event
                                            .get("event")
                                            .and_then(Value::as_str)
                                            .unwrap_or_default();
                                        let params = event.get("params");

                                        match event_name {
                                            "app.start" => {
                                                if let Some(app_id) = params
                                                    .and_then(|p| p.get("appId"))
                                                    .and_then(Value::as_str)
                                                {
                                                    state.write().await.app_id = Some(app_id.to_string());
                                                }
                                            }
                                            "app.debugPort" => {
                                                if let Some(ws_uri) = params
                                                    .and_then(|p| p.get("wsUri"))
                                                    .and_then(Value::as_str)
                                                {
                                                    let devtools_url = build_devtools_performance_url(ws_uri);
                                                    {
                                                        let mut guard = state.write().await;
                                                        guard.vm_service_uri = Some(ws_uri.to_string());
                                                        if guard.devtools_uri.is_none() {
                                                            guard.devtools_uri = devtools_url.clone();
                                                        }
                                                    }
                                                    append_log(
                                                        &out_log_path,
                                                        &format!("[vm-service] {ws_uri}\n"),
                                                    );

                                                    if mode == FlutterRunMode::Profile && !devtools_launched {
                                                        devtools_launched = true;
                                                        launch_profile_devtools(
                                                            &client_clone,
                                                            &project_root,
                                                            ws_uri,
                                                            devtools_url.as_deref(),
                                                            custom_sdk_owned.as_deref(),
                                                        )
                                                        .await;
                                                    }
                                                    let _ = client_clone.send_request::<CodeLensRefresh>(()).await;
                                                }
                                            }
                                            "app.started" => {
                                                if !started_notified {
                                                    started_notified = true;
                                                    progress.report("Application running").await;
                                                    let _ = client_clone.send_request::<CodeLensRefresh>(()).await;
                                                }
                                            }
                                            "app.log" => {
                                                if let Some(msg) = params
                                                    .and_then(|p| p.get("log"))
                                                    .and_then(Value::as_str)
                                                {
                                                    append_log(&out_log_path, &format!("{msg}\n"));
                                                }
                                            }
                                            "app.progress" => {
                                                if let Some(msg) = params
                                                    .and_then(|p| p.get("message"))
                                                    .and_then(Value::as_str)
                                                {
                                                    progress.report(msg).await;
                                                    append_log(&out_log_path, &format!("[progress] {msg}\n"));
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                } else if !clean_line.trim().is_empty() {
                                    append_log(&out_log_path, &clean_line);
                                }
                            }
                        }
                    }
                }
            }

            let exit_code = if user_stopped {
                0
            } else {
                child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1)
            };

            stdin_task.abort();
            stderr_task.abort();

            append_log(
                &out_log_path,
                &format!(
                    "\n{}\n",
                    if user_stopped {
                        "[flutter-debugger] Application stopped."
                    } else {
                        "[flutter-debugger] Process exited."
                    }
                ),
            );

            sessions_ref.remove(&key);
            progress
                .end(Some(if user_stopped {
                    "Stopped".to_string()
                } else {
                    "Session ended".to_string()
                }))
                .await;
            let _ = client_clone.send_request::<CodeLensRefresh>(()).await;

            if !user_stopped && exit_code != 0 && !started_notified {
                if let Some(uri) = file_path_to_uri(&out_log_path) {
                    let _ = client_clone
                        .send_request::<ShowDocument>(ShowDocumentParams {
                            uri,
                            external: Some(false),
                            take_focus: Some(true),
                            selection: None,
                        })
                        .await;
                }
                client_clone
                    .show_message(
                        MessageType::ERROR,
                        format!(
                            "Flutter ({}) exited with code {exit_code}. See log tab for details.",
                            mode.as_str()
                        ),
                    )
                    .await;
            }
        });

        Ok(log_path)
    }

    pub async fn trigger_reload(
        &self,
        project_root: &Path,
        full_restart: bool,
    ) -> Result<String, String> {
        let key = normalize_project_key(project_root);
        let (state, tx, rpc_id) = {
            let entry = self
                .sessions
                .get(&key)
                .ok_or_else(|| "No managed Flutter session running".to_string())?;
            (
                Arc::clone(&entry.state),
                entry.stdin_tx.clone(),
                entry.next_rpc_id.fetch_add(1, Ordering::Relaxed),
            )
        };

        let guard = state.read().await;
        if guard.mode != FlutterRunMode::Debug {
            return Err(format!(
                "Hot Reload/Restart is disabled in `{}` mode",
                guard.mode.as_str()
            ));
        }
        let app_id = guard
            .app_id
            .clone()
            .ok_or_else(|| "Flutter app is still starting; try again in a moment".to_string())?;
        drop(guard);

        let payload = json!([{
            "method": "app.restart",
            "id": rpc_id,
            "params": {
                "appId": app_id,
                "fullRestart": full_restart,
                "pause": false
            }
        }]);
        let mut line = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
        line.push('\n');

        tx.send(line)
            .await
            .map_err(|_| "Managed Flutter session is closed".to_string())?;

        Ok(if full_restart {
            "Hot Restart triggered".to_string()
        } else {
            "Hot Reload triggered".to_string()
        })
    }

    pub async fn toggle_debug_overlay(
        &self,
        project_root: &Path,
        toggle: DebugOverlayToggle,
    ) -> Result<String, String> {
        let key = normalize_project_key(project_root);
        let (state, tx, rpc_id) = {
            let entry = self
                .sessions
                .get(&key)
                .ok_or_else(|| "No managed Flutter session running".to_string())?;
            (
                Arc::clone(&entry.state),
                entry.stdin_tx.clone(),
                entry.next_rpc_id.fetch_add(1, Ordering::Relaxed),
            )
        };

        let mut guard = state.write().await;
        if guard.mode == FlutterRunMode::Release {
            return Err("Visual debug overlays are unavailable in release mode".to_string());
        }
        let app_id = guard
            .app_id
            .clone()
            .ok_or_else(|| "Flutter app is still starting; try again in a moment".to_string())?;

        let (method_name, params, status_text) = match toggle {
            DebugOverlayToggle::PerformanceOverlay => {
                guard.overlays.performance_overlay = !guard.overlays.performance_overlay;
                let v = guard.overlays.performance_overlay;
                (
                    "ext.flutter.showPerformanceOverlay",
                    json!({ "enabled": v.to_string() }),
                    format!("{}: {}", toggle.label(), if v { "ON" } else { "OFF" }),
                )
            }
            DebugOverlayToggle::DebugPaint => {
                guard.overlays.debug_paint = !guard.overlays.debug_paint;
                let v = guard.overlays.debug_paint;
                (
                    "ext.flutter.debugPaint",
                    json!({ "enabled": v.to_string() }),
                    format!("{}: {}", toggle.label(), if v { "ON" } else { "OFF" }),
                )
            }
            DebugOverlayToggle::RepaintRainbow => {
                guard.overlays.repaint_rainbow = !guard.overlays.repaint_rainbow;
                let v = guard.overlays.repaint_rainbow;
                (
                    "ext.flutter.repaintRainbow",
                    json!({ "enabled": v.to_string() }),
                    format!("{}: {}", toggle.label(), if v { "ON" } else { "OFF" }),
                )
            }
            DebugOverlayToggle::SlowAnimations => {
                guard.overlays.slow_animations = !guard.overlays.slow_animations;
                let v = guard.overlays.slow_animations;
                (
                    "ext.flutter.timeDilation",
                    json!({ "timeDilation": if v { "5.0" } else { "1.0" } }),
                    format!("{}: {}", toggle.label(), if v { "ON" } else { "OFF" }),
                )
            }
            DebugOverlayToggle::DebugBanner => {
                guard.overlays.debug_banner = !guard.overlays.debug_banner;
                let v = guard.overlays.debug_banner;
                (
                    "ext.flutter.debugAllowBanner",
                    json!({ "enabled": v.to_string() }),
                    format!("{}: {}", toggle.label(), if v { "ON" } else { "OFF" }),
                )
            }
            DebugOverlayToggle::PlatformOverride => {
                const PLATFORMS: [&str; 5] = ["android", "iOS", "macOS", "linux", "windows"];
                let idx = guard.overlays.platform_index % PLATFORMS.len();
                let platform = PLATFORMS[idx];
                guard.overlays.platform_index = (idx + 1) % PLATFORMS.len();
                (
                    "ext.flutter.platformOverride",
                    json!({ "value": platform }),
                    format!("{}: {platform}", toggle.label()),
                )
            }
        };
        drop(guard);

        let payload = json!([{
            "method": "app.callServiceExtension",
            "id": rpc_id,
            "params": {
                "appId": app_id,
                "methodName": method_name,
                "params": params
            }
        }]);
        let mut line = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
        line.push('\n');

        tx.send(line)
            .await
            .map_err(|_| "Managed Flutter session is closed".to_string())?;

        Ok(status_text)
    }

    pub async fn stop_app(&self, project_root: &Path) -> Result<String, String> {
        let key = normalize_project_key(project_root);
        let handle = self
            .sessions
            .remove(&key)
            .map(|(_, h)| h)
            .ok_or_else(|| "No running Flutter app found for this project".to_string())?;

        stop_handle(handle).await;
        Ok("Stopped Flutter application".to_string())
    }

    /// Gracefully stops and force-kills all running Flutter apps on LSP shutdown.
    pub async fn stop_all(&self) {
        let keys: Vec<String> = self.sessions.iter().map(|e| e.key().clone()).collect();
        for key in keys {
            if let Some((_, handle)) = self.sessions.remove(&key) {
                stop_handle(handle).await;
            }
        }
    }
}

async fn stop_handle(handle: ManagedAppHandle) {
    let app_id = handle.state.read().await.app_id.clone();
    if let Some(app_id) = app_id {
        let rpc_id = handle.next_rpc_id.fetch_add(1, Ordering::Relaxed);
        let payload = json!([{
            "method": "app.stop",
            "id": rpc_id,
            "params": { "appId": app_id }
        }]);
        if let Ok(mut line) = serde_json::to_string(&payload) {
            line.push('\n');
            let _ = handle.stdin_tx.send(line).await;
        }
    }

    if let Some(stop_tx) = handle.stop_tx.lock().await.take() {
        let _ = stop_tx.send(());
    }
}

/// Builds the top-of-file control bar CodeLens rendered directly on line 1 of an active
/// `flutter-run-*.log` tab so users always have one-click Stop / Hot Reload / DevTools.
pub fn build_run_log_code_lenses(uri: &Uri, snapshot: &ManagedAppSnapshot) -> Vec<CodeLens> {
    let top_range = Range {
        start: Position::new(0, 0),
        end: Position::new(0, 0),
    };
    let uri_str = uri.as_str().to_string();
    let mut lenses = Vec::new();

    if snapshot.mode == FlutterRunMode::Debug {
        lenses.push(make_lens(
            top_range,
            "Hot Reload",
            "flutterDebugger.hotReload",
            vec![json!(uri_str)],
        ));
        lenses.push(make_lens(
            top_range,
            "Hot Restart",
            "flutterDebugger.hotRestart",
            vec![json!(uri_str)],
        ));
    }

    if snapshot.devtools_uri.is_some() || snapshot.vm_service_uri.is_some() {
        let dt_label = if snapshot.mode == FlutterRunMode::Profile {
            "Performance"
        } else {
            "DevTools"
        };
        lenses.push(make_lens(
            top_range,
            dt_label,
            "flutterDebugger.openDevTools",
            vec![json!(uri_str)],
        ));
    }

    lenses.push(make_lens(
        top_range,
        "Stop",
        "flutterDebugger.stopApp",
        vec![json!(uri_str)],
    ));

    lenses
}

fn make_lens(range: Range, title: &str, command: &str, arguments: Vec<Value>) -> CodeLens {
    CodeLens {
        range,
        command: Some(LspCommand {
            title: title.to_string(),
            command: command.to_string(),
            arguments: Some(arguments),
        }),
        data: None,
    }
}

async fn terminate_child_tree(child: &mut Child) {
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }

    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let _ = Command::new("pkill")
            .args(["-TERM", "-P", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }

    let _ = child.kill().await;
    let _ = child.wait().await;
}

fn spawn_flutter_run(
    executable: &str,
    args: &[String],
    cwd: &Path,
) -> std::io::Result<Child> {
    #[cfg(windows)]
    {
        let lower = executable.to_ascii_lowercase();
        if lower.ends_with(".bat") || lower.ends_with(".cmd") {
            return Command::new("cmd")
                .arg("/C")
                .arg(executable)
                .args(args)
                .current_dir(cwd)
                .kill_on_drop(true)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
        }
    }

    Command::new(executable)
        .args(args)
        .current_dir(cwd)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// Parses a `flutter run --machine` JSON array line (ignoring plain text banners).
pub fn parse_daemon_events(line: &str) -> Option<Vec<Value>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return None;
    }
    serde_json::from_str::<Vec<Value>>(trimmed).ok()
}

/// Builds a Dart DevTools Performance page URL from a VM Service URI when served by DDS.
pub fn build_devtools_performance_url(ws_uri: &str) -> Option<String> {
    let parsed = url::Url::parse(ws_uri).ok()?;
    let host = parsed.host_str()?;
    let port = parsed.port()?;
    let path = parsed.path().trim_end_matches("/ws").trim_end_matches('/');
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("uri", ws_uri);
    let query = serializer.finish();
    Some(format!("http://{host}:{port}{path}/devtools/#/performance?{query}"))
}

async fn launch_profile_devtools(
    client: &Client,
    project_root: &Path,
    ws_uri: &str,
    devtools_url: Option<&str>,
    custom_sdk_path: Option<&str>,
) {
    if let Some(url_str) = devtools_url
        && let Ok(uri) = url_str.parse::<Uri>()
    {
        let _ = client
            .send_request::<ShowDocument>(ShowDocumentParams {
                uri,
                external: Some(true),
                take_focus: Some(true),
                selection: None,
            })
            .await;
        client
            .show_message(
                MessageType::INFO,
                "Profile mode started - opened Flutter DevTools Performance view.".to_string(),
            )
            .await;
        return;
    }

    let dart_exe = resolve_sdk_executable(project_root, ProjectKind::Dart, custom_sdk_path);
    let _ = Command::new(dart_exe)
        .args(["devtools", &format!("--vm-uri={ws_uri}")])
        .current_dir(project_root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn append_log(path: &Path, content: &str) {
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(content.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_daemon_event_array_and_ignores_banners() {
        assert!(parse_daemon_events("Launching lib/main.dart...").is_none());
        let events = parse_daemon_events(
            r#"[{"event":"app.debugPort","params":{"appId":"1","port":50000,"wsUri":"ws://localhost:50000/abc/ws"}}]"#,
        )
        .expect("parsed");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event"], "app.debugPort");
    }

    #[test]
    fn builds_devtools_performance_url_from_ws_uri() {
        let url = build_devtools_performance_url("ws://localhost:50000/abc/ws")
            .expect("devtools url");
        assert!(url.starts_with("http://localhost:50000/abc/devtools/#/performance?uri="));
    }

    #[test]
    fn builds_log_control_bar_code_lenses_by_mode() {
        let uri: Uri = "file:///tmp/flutter-run-debug-1.log".parse().expect("uri");
        let debug_snap = ManagedAppSnapshot {
            project_root: PathBuf::from("/tmp/app"),
            mode: FlutterRunMode::Debug,
            vm_service_uri: Some("ws://localhost:50000/ws".into()),
            devtools_uri: Some("http://localhost:50000/devtools".into()),
        };
        let debug_lenses = build_run_log_code_lenses(&uri, &debug_snap);
        let debug_titles: Vec<&str> = debug_lenses
            .iter()
            .filter_map(|l| l.command.as_ref().map(|c| c.title.as_str()))
            .collect();
        assert_eq!(
            debug_titles,
            vec!["Hot Reload", "Hot Restart", "DevTools", "Stop"]
        );

        let release_snap = ManagedAppSnapshot {
            project_root: PathBuf::from("/tmp/app"),
            mode: FlutterRunMode::Release,
            vm_service_uri: None,
            devtools_uri: None,
        };
        let release_lenses = build_run_log_code_lenses(&uri, &release_snap);
        let release_titles: Vec<&str> = release_lenses
            .iter()
            .filter_map(|l| l.command.as_ref().map(|c| c.title.as_str()))
            .collect();
        assert_eq!(release_titles, vec!["Stop"]);
    }
}
