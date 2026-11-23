pub mod editor;
pub mod ipc_client;
pub mod models;
pub mod session;
pub mod vm_client;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use async_channel::{Receiver, Sender};
use serde_json::json;
use tokio::sync::mpsc;

pub use models::{
    DevToolsPage, FrameTiming, HttpRequestDetail, HttpRequestRecord, JsonTokenKind, LogEntry,
    LogKind, MemorySnapshot, OverlayState, ServiceOverlay, SourceLocation, WidgetProperty,
    WidgetTreeNode, format_and_tokenize_json,
};
pub use session::ActiveSessionRecord;

use editor::open_source_in_zed;
use ipc_client::{IpcRequest, send_ipc_to_session};
use session::{default_sessions_dir, list_active_sessions_in, select_best_session};
use vm_client::{DartVmClient, VmPushEvent};

#[derive(Debug, Clone, Default)]
pub struct CliLaunchArgs {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub vm_uri: Option<String>,
    pub page: Option<DevToolsPage>,
}

impl CliLaunchArgs {
    pub fn parse(args: &[String]) -> Self {
        let mut out = Self::default();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--project" | "-p" => {
                    out.project = iter.next().cloned();
                }
                "--session" | "-s" => {
                    out.session_id = iter.next().cloned();
                }
                "--vm-uri" | "--uri" => {
                    out.vm_uri = iter.next().cloned();
                }
                "--page" | "--tab" => {
                    if let Some(val) = iter.next() {
                        out.page = DevToolsPage::parse(val);
                    }
                }
                other => {
                    if let Some(v) = other.strip_prefix("--project=") {
                        out.project = Some(v.to_string());
                    } else if let Some(v) = other.strip_prefix("--session=") {
                        out.session_id = Some(v.to_string());
                    } else if let Some(v) = other.strip_prefix("--vm-uri=") {
                        out.vm_uri = Some(v.to_string());
                    } else if let Some(v) = other.strip_prefix("--page=") {
                        out.page = DevToolsPage::parse(v);
                    }
                }
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    WaitingForSession,
    Connecting(String),
    Connected {
        vm_uri: String,
        isolate_name: String,
        target_cpu: String,
        arch_bits: u64,
    },
    Disconnected(String),
}

/// Commands sent from the GPUI main thread to the background Tokio worker.
#[derive(Debug, Clone)]
pub enum BackendCommand {
    SelectSession {
        session_id: String,
    },
    ConnectDirectVmUri {
        vm_uri: String,
    },
    SetPreferredProject {
        project: Option<String>,
        session_id: Option<String>,
    },
    HotReload,
    HotRestart,
    ToggleOverlay(ServiceOverlay),
    TakeScreenshot,
    OpenWebDevTools {
        page: Option<String>,
    },
    SetNetworkRecording(bool),
    ClearNetworkProfile,
    FetchHttpRequestDetail {
        request_id: String,
    },
    RefreshWidgetTree,
    SelectWidgetNode {
        value_id: String,
    },
    SetMemoryPolling(bool),
    ForceGc,
    OpenInZed(SourceLocation),
    Shutdown,
}

/// Events pushed from the background Tokio worker to the GPUI main thread.
#[derive(Debug, Clone)]
pub enum BackendEvent {
    SessionsUpdated {
        sessions: Vec<ActiveSessionRecord>,
        active_session_id: Option<String>,
    },
    ConnectionChanged(ConnectionStatus),
    Toast {
        message: String,
        is_error: bool,
    },
    OverlayStateUpdated(OverlayState),
    HttpRequestsBatch(Vec<HttpRequestRecord>),
    HttpRequestDetailLoaded {
        request_id: String,
        detail: HttpRequestDetail,
    },
    WidgetTreeLoaded(WidgetTreeNode),
    WidgetPropertiesLoaded {
        value_id: String,
        properties: Vec<WidgetProperty>,
    },
    LogAppended(LogEntry),
    FrameAppended(FrameTiming),
    MemoryAppended(MemorySnapshot),
}

pub fn spawn_backend_worker(
    initial_args: CliLaunchArgs,
) -> (Sender<BackendCommand>, Receiver<BackendEvent>) {
    let (cmd_tx, cmd_rx) = async_channel::unbounded::<BackendCommand>();
    let (event_tx, event_rx) = async_channel::unbounded::<BackendEvent>();

    std::thread::Builder::new()
        .name("flutter-devtools-tokio-worker".to_string())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("Failed to initialize background Tokio runtime");
            rt.block_on(run_worker_loop(initial_args, cmd_rx, event_tx));
        })
        .expect("Failed to spawn background Tokio thread");

    (cmd_tx, event_rx)
}

struct WorkerState {
    sessions: Vec<ActiveSessionRecord>,
    active_session: Option<ActiveSessionRecord>,
    preferred_project: Option<String>,
    preferred_session_id: Option<String>,
    explicit_vm_uri: Option<String>,
    connected_vm_uri: Option<String>,
    vm_client: Option<DartVmClient>,
    overlays: OverlayState,
    network_recording: bool,
    memory_polling: bool,
    http_updated_since: Option<u64>,
    log_seq: Arc<AtomicU64>,
}

async fn run_worker_loop(
    initial_args: CliLaunchArgs,
    cmd_rx: Receiver<BackendCommand>,
    event_tx: Sender<BackendEvent>,
) {
    let (vm_push_tx, mut vm_push_rx) = mpsc::unbounded_channel::<VmPushEvent>();

    let mut state = WorkerState {
        sessions: Vec::new(),
        active_session: None,
        preferred_project: initial_args.project,
        preferred_session_id: initial_args.session_id,
        explicit_vm_uri: initial_args.vm_uri,
        connected_vm_uri: None,
        vm_client: None,
        overlays: OverlayState::default(),
        network_recording: true,
        memory_polling: true,
        http_updated_since: None,
        log_seq: Arc::new(AtomicU64::new(1)),
    };

    // Perform immediate session discovery & connection attempt
    scan_sessions_and_reconcile(&mut state, &event_tx, &vm_push_tx).await;

    let mut session_scan_ticker = tokio::time::interval(Duration::from_secs(1));
    let mut http_poll_ticker = tokio::time::interval(Duration::from_millis(500));
    let mut memory_poll_ticker = tokio::time::interval(Duration::from_secs(1));

    loop {
        tokio::select! {
            cmd_res = cmd_rx.recv() => {
                let Ok(cmd) = cmd_res else {
                    break;
                };
                if matches!(cmd, BackendCommand::Shutdown) {
                    break;
                }
                handle_backend_command(cmd, &mut state, &event_tx, &vm_push_tx).await;
            }
            Some(push_ev) = vm_push_rx.recv() => {
                handle_vm_push_event(push_ev, &mut state, &event_tx).await;
            }
            _ = session_scan_ticker.tick() => {
                scan_sessions_and_reconcile(&mut state, &event_tx, &vm_push_tx).await;
            }
            _ = http_poll_ticker.tick() => {
                if state.network_recording
                    && let Some(client) = state.vm_client.clone()
                    && let Ok((ts, batch)) = client.poll_http_profile(state.http_updated_since).await
                {
                    if ts > 0 {
                        state.http_updated_since = Some(ts);
                    }
                    if !batch.is_empty() {
                        let _ = event_tx.try_send(BackendEvent::HttpRequestsBatch(batch));
                    }
                }
            }
            _ = memory_poll_ticker.tick() => {
                if state.memory_polling
                    && let Some(client) = state.vm_client.clone()
                    && let Ok(snap) = client.poll_memory_usage(false).await
                {
                    let _ = event_tx.try_send(BackendEvent::MemoryAppended(snap));
                }
            }
        }
    }
}

async fn scan_sessions_and_reconcile(
    state: &mut WorkerState,
    event_tx: &Sender<BackendEvent>,
    vm_push_tx: &mpsc::UnboundedSender<VmPushEvent>,
) {
    let dir = default_sessions_dir();
    let sessions = list_active_sessions_in(&dir);

    let best = select_best_session(
        &sessions,
        state.preferred_session_id.as_deref(),
        state.preferred_project.as_deref(),
    )
    .cloned();

    let active_id = best.as_ref().map(|s| s.session_id.clone());
    let prev_id = state.active_session.as_ref().map(|s| s.session_id.clone());

    if sessions != state.sessions || active_id != prev_id {
        state.sessions = sessions.clone();
        state.active_session = best.clone();
        let _ = event_tx.try_send(BackendEvent::SessionsUpdated {
            sessions,
            active_session_id: active_id,
        });
    } else if let Some(ref current_best) = best {
        // Keep active_session updated when `vm_service_uri` populates after `app.debugPort`
        state.active_session = Some(current_best.clone());
    }

    let desired_vm_uri = state.explicit_vm_uri.clone().or_else(|| {
        state
            .active_session
            .as_ref()
            .and_then(|s| s.vm_service_uri.clone())
    });

    match desired_vm_uri {
        Some(uri) if !uri.trim().is_empty() => {
            if state.vm_client.is_none() || state.connected_vm_uri.as_deref() != Some(uri.as_str())
            {
                connect_to_vm(&uri, state, event_tx, vm_push_tx).await;
            }
        }
        _ => {
            if state.vm_client.is_some() {
                state.vm_client = None;
                state.connected_vm_uri = None;
                let _ = event_tx.try_send(BackendEvent::ConnectionChanged(
                    ConnectionStatus::WaitingForSession,
                ));
            }
        }
    }
}

async fn connect_to_vm(
    raw_uri: &str,
    state: &mut WorkerState,
    event_tx: &Sender<BackendEvent>,
    vm_push_tx: &mpsc::UnboundedSender<VmPushEvent>,
) {
    let _ = event_tx.try_send(BackendEvent::ConnectionChanged(
        ConnectionStatus::Connecting(raw_uri.to_string()),
    ));

    match DartVmClient::connect(raw_uri, vm_push_tx.clone(), Arc::clone(&state.log_seq)).await {
        Ok((client, meta)) => {
            state.connected_vm_uri = Some(raw_uri.to_string());
            state.http_updated_since = None;
            state.vm_client = Some(client.clone());

            let _ = event_tx.try_send(BackendEvent::ConnectionChanged(
                ConnectionStatus::Connected {
                    vm_uri: client.ws_uri().to_string(),
                    isolate_name: meta.main_isolate_name,
                    target_cpu: meta.target_cpu,
                    arch_bits: meta.arch_bits,
                },
            ));

            // Initial fetch of HTTP profile, Memory snapshot, and Widget Tree
            if let Ok((ts, batch)) = client.poll_http_profile(None).await {
                if ts > 0 {
                    state.http_updated_since = Some(ts);
                }
                if !batch.is_empty() {
                    let _ = event_tx.try_send(BackendEvent::HttpRequestsBatch(batch));
                }
            }
            if let Ok(mem) = client.poll_memory_usage(false).await {
                let _ = event_tx.try_send(BackendEvent::MemoryAppended(mem));
            }
            if let Ok(tree) = client.fetch_widget_summary_tree("devtools-gpui-tree").await {
                let _ = event_tx.try_send(BackendEvent::WidgetTreeLoaded(tree));
            }
        }
        Err(err) => {
            state.vm_client = None;
            state.connected_vm_uri = None;
            let _ = event_tx.try_send(BackendEvent::ConnectionChanged(
                ConnectionStatus::Disconnected(err),
            ));
        }
    }
}

async fn handle_backend_command(
    cmd: BackendCommand,
    state: &mut WorkerState,
    event_tx: &Sender<BackendEvent>,
    vm_push_tx: &mpsc::UnboundedSender<VmPushEvent>,
) {
    match cmd {
        BackendCommand::SelectSession { session_id } => {
            state.preferred_session_id = Some(session_id);
            state.explicit_vm_uri = None;
            scan_sessions_and_reconcile(state, event_tx, vm_push_tx).await;
        }
        BackendCommand::ConnectDirectVmUri { vm_uri } => {
            state.explicit_vm_uri = Some(vm_uri.clone());
            connect_to_vm(&vm_uri, state, event_tx, vm_push_tx).await;
        }
        BackendCommand::SetPreferredProject {
            project,
            session_id,
        } => {
            if project.is_some() {
                state.preferred_project = project;
            }
            if session_id.is_some() {
                state.preferred_session_id = session_id;
            }
            scan_sessions_and_reconcile(state, event_tx, vm_push_tx).await;
        }
        BackendCommand::HotReload => {
            if let Some(ref session) = state.active_session {
                let started = std::time::Instant::now();
                match send_ipc_to_session(session, &IpcRequest::HotReload).await {
                    Ok(resp) => {
                        let ms = resp
                            .duration_ms
                            .unwrap_or_else(|| started.elapsed().as_millis() as u64);
                        let msg = if resp.ok {
                            format!("⚡ Hot Reload: {ms}ms")
                        } else {
                            resp.message
                        };
                        let _ = event_tx.try_send(BackendEvent::Toast {
                            message: msg,
                            is_error: !resp.ok,
                        });
                        if resp.ok
                            && let Some(ref client) = state.vm_client
                            && let Ok(tree) =
                                client.fetch_widget_summary_tree("devtools-gpui-tree").await
                        {
                            let _ = event_tx.try_send(BackendEvent::WidgetTreeLoaded(tree));
                        }
                    }
                    Err(err) => {
                        let _ = event_tx.try_send(BackendEvent::Toast {
                            message: format!("Hot Reload failed: {err}"),
                            is_error: true,
                        });
                    }
                }
            } else {
                let _ = event_tx.try_send(BackendEvent::Toast {
                    message: "No active Flutter session for Hot Reload".to_string(),
                    is_error: true,
                });
            }
        }
        BackendCommand::HotRestart => {
            if let Some(ref session) = state.active_session {
                let started = std::time::Instant::now();
                match send_ipc_to_session(session, &IpcRequest::HotRestart).await {
                    Ok(resp) => {
                        let ms = resp
                            .duration_ms
                            .unwrap_or_else(|| started.elapsed().as_millis() as u64);
                        let msg = if resp.ok {
                            format!("🔄 Hot Restart: {ms}ms")
                        } else {
                            resp.message
                        };
                        let _ = event_tx.try_send(BackendEvent::Toast {
                            message: msg,
                            is_error: !resp.ok,
                        });
                    }
                    Err(err) => {
                        let _ = event_tx.try_send(BackendEvent::Toast {
                            message: format!("Hot Restart failed: {err}"),
                            is_error: true,
                        });
                    }
                }
            } else {
                let _ = event_tx.try_send(BackendEvent::Toast {
                    message: "No active Flutter session for Hot Restart".to_string(),
                    is_error: true,
                });
            }
        }
        BackendCommand::ToggleOverlay(overlay) => {
            if let Some(ref session) = state.active_session {
                match send_ipc_to_session(session, &IpcRequest::ToggleOverlay { overlay }).await {
                    Ok(resp) => {
                        if resp.ok {
                            let _ = state.overlays.next_toggle_params(overlay);
                            let _ = event_tx.try_send(BackendEvent::OverlayStateUpdated(
                                state.overlays.clone(),
                            ));
                        }
                        let _ = event_tx.try_send(BackendEvent::Toast {
                            message: resp.message,
                            is_error: !resp.ok,
                        });
                    }
                    Err(err) => {
                        let _ = event_tx.try_send(BackendEvent::Toast {
                            message: format!("Overlay toggle failed: {err}"),
                            is_error: true,
                        });
                    }
                }
            } else if let Some(ref client) = state.vm_client {
                let (method, params) = state.overlays.next_toggle_params(overlay);
                match client.call_isolate(method, params).await {
                    Ok(_) => {
                        let _ = event_tx
                            .try_send(BackendEvent::OverlayStateUpdated(state.overlays.clone()));
                        let _ = event_tx.try_send(BackendEvent::Toast {
                            message: format!(
                                "{}: {}",
                                overlay.label(),
                                state.overlays.badge_text(overlay)
                            ),
                            is_error: false,
                        });
                    }
                    Err(err) => {
                        let _ = event_tx.try_send(BackendEvent::Toast {
                            message: format!("Failed to toggle {}: {err}", overlay.label()),
                            is_error: true,
                        });
                    }
                }
            } else {
                let _ = event_tx.try_send(BackendEvent::Toast {
                    message: "No active Flutter session".to_string(),
                    is_error: true,
                });
            }
        }
        BackendCommand::TakeScreenshot => {
            if let Some(ref session) = state.active_session {
                match send_ipc_to_session(
                    session,
                    &IpcRequest::Screenshot { output_path: None },
                )
                .await
                {
                    Ok(resp) => {
                        let _ = event_tx.try_send(BackendEvent::Toast {
                            message: resp.message,
                            is_error: !resp.ok,
                        });
                    }
                    Err(err) => {
                        let _ = event_tx.try_send(BackendEvent::Toast {
                            message: format!("Screenshot failed: {err}"),
                            is_error: true,
                        });
                    }
                }
            } else {
                let _ = event_tx.try_send(BackendEvent::Toast {
                    message: "No active session to capture screenshot".to_string(),
                    is_error: true,
                });
            }
        }
        BackendCommand::OpenWebDevTools { page } => {
            if let Some(ref session) = state.active_session {
                match send_ipc_to_session(
                    session,
                    &IpcRequest::OpenDevTools {
                        page: page.clone(),
                    },
                )
                .await
                {
                    Ok(resp) => {
                        let _ = event_tx.try_send(BackendEvent::Toast {
                            message: resp.message,
                            is_error: !resp.ok,
                        });
                    }
                    Err(_) => {
                        spawn_ctl_web_devtools(
                            Some(Path::new(&session.project_root)),
                            page.as_deref(),
                        );
                        let _ = event_tx.try_send(BackendEvent::Toast {
                            message: "Launching Web DevTools via flutter-debugger-server..."
                                .to_string(),
                            is_error: false,
                        });
                    }
                }
            } else {
                spawn_ctl_web_devtools(None, page.as_deref());
                let _ = event_tx.try_send(BackendEvent::Toast {
                    message: "No active session for Web DevTools".to_string(),
                    is_error: true,
                });
            }
        }
        BackendCommand::SetNetworkRecording(enabled) => {
            state.network_recording = enabled;
            if let Some(ref client) = state.vm_client {
                let _ = client
                    .call_isolate(
                        "httpEnableTimelineLogging",
                        json!({ "enabled": enabled }),
                    )
                    .await;
            }
        }
        BackendCommand::ClearNetworkProfile => {
            state.http_updated_since = None;
            if let Some(ref client) = state.vm_client {
                let _ = client.clear_http_profile().await;
            }
        }
        BackendCommand::FetchHttpRequestDetail { request_id } => {
            if let Some(ref client) = state.vm_client
                && let Ok(detail) = client.get_http_profile_request(&request_id).await
            {
                let _ = event_tx.try_send(BackendEvent::HttpRequestDetailLoaded {
                    request_id,
                    detail,
                });
            }
        }
        BackendCommand::RefreshWidgetTree => {
            if let Some(ref client) = state.vm_client {
                match client.fetch_widget_summary_tree("devtools-gpui-tree").await {
                    Ok(tree) => {
                        let _ = event_tx.try_send(BackendEvent::WidgetTreeLoaded(tree));
                    }
                    Err(err) => {
                        let _ = event_tx.try_send(BackendEvent::Toast {
                            message: format!("Inspector tree refresh failed: {err}"),
                            is_error: true,
                        });
                    }
                }
            }
        }
        BackendCommand::SelectWidgetNode { value_id } => {
            if let Some(ref client) = state.vm_client
                && let Ok(properties) = client
                    .fetch_widget_properties(&value_id, "devtools-gpui-props")
                    .await
            {
                let _ = event_tx.try_send(BackendEvent::WidgetPropertiesLoaded {
                    value_id,
                    properties,
                });
            }
        }
        BackendCommand::SetMemoryPolling(enabled) => {
            state.memory_polling = enabled;
        }
        BackendCommand::ForceGc => {
            if let Some(ref client) = state.vm_client {
                match client.trigger_force_gc().await {
                    Ok(snap) => {
                        let _ = event_tx.try_send(BackendEvent::MemoryAppended(snap));
                        let _ = event_tx.try_send(BackendEvent::Toast {
                            message: "Garbage Collection completed".to_string(),
                            is_error: false,
                        });
                    }
                    Err(err) => {
                        let _ = event_tx.try_send(BackendEvent::Toast {
                            message: format!("GC failed: {err}"),
                            is_error: true,
                        });
                    }
                }
            }
        }
        BackendCommand::OpenInZed(loc) => {
            let root = state
                .active_session
                .as_ref()
                .map(|s| PathBuf::from(&s.project_root))
                .or_else(|| state.preferred_project.as_ref().map(PathBuf::from));
            match open_source_in_zed(&loc, root.as_deref()) {
                Ok(msg) => {
                    let _ = event_tx.try_send(BackendEvent::Toast {
                        message: msg,
                        is_error: false,
                    });
                }
                Err(err) => {
                    let _ = event_tx.try_send(BackendEvent::Toast {
                        message: err,
                        is_error: true,
                    });
                }
            }
        }
        BackendCommand::Shutdown => {}
    }
}

async fn handle_vm_push_event(
    ev: VmPushEvent,
    state: &mut WorkerState,
    event_tx: &Sender<BackendEvent>,
) {
    match ev {
        VmPushEvent::Frame(frame) => {
            let _ = event_tx.try_send(BackendEvent::FrameAppended(frame));
        }
        VmPushEvent::Log(entry) => {
            let _ = event_tx.try_send(BackendEvent::LogAppended(entry));
        }
        VmPushEvent::ServiceExtensionStateChanged { extension, value } => {
            state.overlays.sync_from_extension_event(&extension, &value);
            let _ = event_tx.try_send(BackendEvent::OverlayStateUpdated(state.overlays.clone()));
        }
        VmPushEvent::ServiceExtensionAdded { rpc } => {
            if rpc == "ext.flutter.inspector.getRootWidgetSummaryTree"
                && let Some(ref client) = state.vm_client
                && let Ok(tree) = client.fetch_widget_summary_tree("devtools-gpui-tree").await
            {
                let _ = event_tx.try_send(BackendEvent::WidgetTreeLoaded(tree));
            }
        }
        VmPushEvent::IsolateReloaded => {
            if let Some(ref client) = state.vm_client {
                let _ = client.refresh_main_isolate().await;
                if let Ok(tree) = client.fetch_widget_summary_tree("devtools-gpui-tree").await {
                    let _ = event_tx.try_send(BackendEvent::WidgetTreeLoaded(tree));
                }
            }
        }
        VmPushEvent::GcNotification => {
            if let Some(ref client) = state.vm_client
                && let Ok(snap) = client.poll_memory_usage(true).await
            {
                let _ = event_tx.try_send(BackendEvent::MemoryAppended(snap));
            }
        }
        VmPushEvent::Disconnected(reason) => {
            state.vm_client = None;
            state.connected_vm_uri = None;
            let _ = event_tx.try_send(BackendEvent::ConnectionChanged(
                ConnectionStatus::Disconnected(reason),
            ));
        }
    }
}

fn spawn_ctl_web_devtools(project_root: Option<&Path>, page: Option<&str>) {
    let server_bin = dirs::home_dir()
        .map(|h| h.join(".local/bin/flutter-debugger-server"))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("flutter-debugger-server"));

    let mut cmd = Command::new(server_bin);
    cmd.arg("ctl").arg("devtools");
    if let Some(root) = project_root {
        cmd.arg("--project").arg(root);
    }
    if let Some(p) = page {
        cmd.arg("--page").arg(p);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _ = cmd.spawn();
}
