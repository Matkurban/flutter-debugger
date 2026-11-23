use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender};
use flutter_debugger_protocol::{
    ActiveSessionRecord, IpcRequest, OverlayState, default_sessions_dir, list_active_sessions_in,
    select_best_session,
};
use tokio::sync::mpsc;

use crate::backend::command::{BackendCommand, BackendEvent, ConnectionStatus};
use crate::backend::editor::open_source_in_zed;
use crate::backend::ipc_client::send_ipc_to_session;
use crate::backend::vm_client::{DartVmClient, VmPushEvent};
use crate::cli::CliLaunchArgs;
use crate::protocol::{LogEntry, ParsedLogRecord};

pub fn spawn_backend_worker(initial_args: CliLaunchArgs) -> (Sender<BackendCommand>, Receiver<BackendEvent>) {
    let (cmd_tx, cmd_rx) = async_channel::unbounded::<BackendCommand>();
    let (event_tx, event_rx) = async_channel::unbounded::<BackendEvent>();
    std::thread::Builder::new()
        .name("flutter-devtools-tokio-worker".to_string())
        .spawn(move || {
            if let Ok(rt) = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                rt.block_on(run_worker_loop(initial_args, cmd_rx, event_tx));
            }
        })
        .ok();
    (cmd_tx, event_rx)
}

struct WorkerState {
    sessions: Vec<ActiveSessionRecord>, active_session: Option<ActiveSessionRecord>,
    preferred_project: Option<String>, preferred_session_id: Option<String>,
    explicit_vm_uri: Option<String>, connected_vm_uri: Option<String>,
    vm_client: Option<DartVmClient>, overlays: OverlayState,
    network_recording: bool, memory_polling: bool, http_updated_since: Option<u64>,
    log_seq: Arc<AtomicU64>, tree_group_seq: u64, props_group_seq: u64,
    active_tree_group: String, active_props_group: String, last_gc_poll: Option<Instant>,
}

impl WorkerState {
    fn next_tree_group(&mut self) -> (String, String) {
        let prev = std::mem::take(&mut self.active_tree_group);
        self.tree_group_seq += 1;
        let next = format!("devtools-gpui-tree-{}", self.tree_group_seq);
        self.active_tree_group = next.clone();
        (next, prev)
    }
    fn next_props_group(&mut self) -> (String, String) {
        let prev = std::mem::take(&mut self.active_props_group);
        self.props_group_seq += 1;
        let next = format!("devtools-gpui-props-{}", self.props_group_seq);
        self.active_props_group = next.clone();
        (next, prev)
    }
}

async fn run_worker_loop(initial_args: CliLaunchArgs, cmd_rx: Receiver<BackendCommand>, event_tx: Sender<BackendEvent>) {
    let (vm_push_tx, mut vm_push_rx) = mpsc::unbounded_channel::<VmPushEvent>();
    let mut state = WorkerState {
        sessions: Vec::new(), active_session: None,
        preferred_project: initial_args.project, preferred_session_id: initial_args.session_id,
        explicit_vm_uri: initial_args.vm_uri, connected_vm_uri: None, vm_client: None,
        overlays: OverlayState::default(), network_recording: true, memory_polling: true,
        http_updated_since: None, log_seq: Arc::new(AtomicU64::new(1)),
        tree_group_seq: 0, props_group_seq: 0,
        active_tree_group: String::new(), active_props_group: String::new(), last_gc_poll: None,
    };

    scan_sessions_and_reconcile(&mut state, &event_tx, &vm_push_tx).await;
    let mut session_scan_ticker = tokio::time::interval(Duration::from_secs(1));
    let mut http_poll_ticker = tokio::time::interval(Duration::from_millis(500));
    let mut memory_poll_ticker = tokio::time::interval(Duration::from_secs(1));

    loop {
        tokio::select! {
            cmd_res = cmd_rx.recv() => {
                let Ok(cmd) = cmd_res else { break };
                if matches!(cmd, BackendCommand::Shutdown) {
                    if let Some(ref client) = state.vm_client {
                        client.dispose_inspector_group(&state.active_tree_group).await;
                        client.dispose_inspector_group(&state.active_props_group).await;
                    }
                    break;
                }
                handle_backend_command(cmd, &mut state, &event_tx, &vm_push_tx).await;
            }
            Some(push_ev) = vm_push_rx.recv() => handle_vm_push_event(push_ev, &mut state, &event_tx).await,
            _ = session_scan_ticker.tick() => scan_sessions_and_reconcile(&mut state, &event_tx, &vm_push_tx).await,
            _ = http_poll_ticker.tick() => {
                if state.network_recording && let Some(c) = state.vm_client.clone() && let Ok((ts, batch)) = c.poll_http_profile(state.http_updated_since).await {
                    if ts > 0 { state.http_updated_since = Some(ts); }
                    if !batch.is_empty() { let _ = event_tx.try_send(BackendEvent::HttpRequestsBatch(batch)); }
                }
            }
            _ = memory_poll_ticker.tick() => {
                if state.memory_polling && let Some(c) = state.vm_client.clone() && let Ok(snap) = c.poll_memory_usage(false).await {
                    let _ = event_tx.try_send(BackendEvent::MemoryAppended(snap));
                }
            }
        }
    }
}

async fn scan_sessions_and_reconcile(state: &mut WorkerState, event_tx: &Sender<BackendEvent>, vm_push_tx: &mpsc::UnboundedSender<VmPushEvent>) {
    let sessions = list_active_sessions_in(&default_sessions_dir());
    let best = select_best_session(&sessions, state.preferred_session_id.as_deref(), state.preferred_project.as_deref()).cloned();
    let active_id = best.as_ref().map(|s| s.session_id.clone());
    let prev_id = state.active_session.as_ref().map(|s| s.session_id.clone());

    if sessions != state.sessions || active_id != prev_id {
        state.sessions = sessions.clone();
        state.active_session = best.clone();
        let _ = event_tx.try_send(BackendEvent::SessionsUpdated { sessions, active_session_id: active_id });
    } else if let Some(ref current_best) = best {
        state.active_session = Some(current_best.clone());
    }

    let desired_vm_uri = state.explicit_vm_uri.clone().or_else(|| state.active_session.as_ref().and_then(|s| s.vm_service_uri.clone()));
    match desired_vm_uri {
        Some(uri) if !uri.trim().is_empty() => {
            if state.vm_client.is_none() || state.connected_vm_uri.as_deref() != Some(uri.as_str()) {
                connect_to_vm(&uri, state, event_tx, vm_push_tx).await;
            }
        }
        _ => if state.vm_client.is_some() {
            state.vm_client = None;
            state.connected_vm_uri = None;
            let _ = event_tx.try_send(BackendEvent::ConnectionChanged(ConnectionStatus::WaitingForSession));
        },
    }
}

async fn connect_to_vm(raw_uri: &str, state: &mut WorkerState, event_tx: &Sender<BackendEvent>, vm_push_tx: &mpsc::UnboundedSender<VmPushEvent>) {
    let _ = event_tx.try_send(BackendEvent::ConnectionChanged(ConnectionStatus::Connecting(raw_uri.into())));
    match DartVmClient::connect(raw_uri, vm_push_tx.clone(), Arc::clone(&state.log_seq)).await {
        Ok((client, meta)) => {
            state.connected_vm_uri = Some(raw_uri.to_string());
            state.http_updated_since = None;
            state.vm_client = Some(client.clone());
            let _ = event_tx.try_send(BackendEvent::ConnectionChanged(ConnectionStatus::Connected {
                vm_uri: client.ws_uri().to_string(), isolate_name: meta.main_isolate_name,
                target_cpu: meta.target_cpu, arch_bits: meta.arch_bits,
            }));
            let project_root = state.active_session.as_ref().map(|s| PathBuf::from(&s.project_root)).or_else(|| state.preferred_project.as_ref().map(PathBuf::from));
            let (tree_group, prev_tree_group) = state.next_tree_group();
            let base_overlays = state.overlays.clone();
            let tx = event_tx.clone();
            tokio::spawn(async move {
                if let Some(ref root) = project_root { client.set_pub_root_directories(root).await; }
                let synced = client.query_initial_overlay_state(&base_overlays).await;
                let _ = tx.try_send(BackendEvent::OverlayStateUpdated(synced));
                if let Ok((_, batch)) = client.poll_http_profile(None).await && !batch.is_empty() { let _ = tx.try_send(BackendEvent::HttpRequestsBatch(batch)); }
                if let Ok(mem) = client.poll_memory_usage(false).await { let _ = tx.try_send(BackendEvent::MemoryAppended(mem)); }
                if let Ok(tree) = client.fetch_widget_summary_tree(&tree_group).await { let _ = tx.try_send(BackendEvent::WidgetTreeLoaded(tree)); }
                client.dispose_inspector_group(&prev_tree_group).await;
            });
        }
        Err(err) => {
            state.vm_client = None;
            state.connected_vm_uri = None;
            let _ = event_tx.try_send(BackendEvent::ConnectionChanged(ConnectionStatus::Disconnected(err.to_string())));
        }
    }
}

async fn handle_backend_command(cmd: BackendCommand, state: &mut WorkerState, event_tx: &Sender<BackendEvent>, vm_push_tx: &mpsc::UnboundedSender<VmPushEvent>) {
    match cmd {
        BackendCommand::SelectSession { session_id } => { state.preferred_session_id = Some(session_id); state.explicit_vm_uri = None; scan_sessions_and_reconcile(state, event_tx, vm_push_tx).await; }
        BackendCommand::ConnectDirectVmUri { vm_uri } => { state.explicit_vm_uri = Some(vm_uri.clone()); connect_to_vm(&vm_uri, state, event_tx, vm_push_tx).await; }
        BackendCommand::SetPreferredProject { project, session_id } => {
            if project.is_some() { state.preferred_project = project; }
            if session_id.is_some() { state.preferred_session_id = session_id; }
            scan_sessions_and_reconcile(state, event_tx, vm_push_tx).await;
        }
        BackendCommand::HotReload | BackendCommand::HotRestart => {
            let is_restart = matches!(cmd, BackendCommand::HotRestart);
            let Some(session) = state.active_session.clone() else { send_toast(event_tx, "No active Flutter session", true); return; };
            let client_opt = state.vm_client.clone();
            let (tree_group, prev_tree_group) = state.next_tree_group();
            let tx = event_tx.clone();
            tokio::spawn(async move {
                let started = Instant::now();
                let (req, label) = if is_restart { (IpcRequest::HotRestart, "🔄 Hot Restart") } else { (IpcRequest::HotReload, "⚡ Hot Reload") };
                match send_ipc_to_session(&session, &req).await {
                    Ok(resp) => {
                        let ms = resp.duration_ms.unwrap_or_else(|| started.elapsed().as_millis() as u64);
                        send_toast(&tx, if resp.ok { format!("{label}: {ms}ms") } else { resp.message }, !resp.ok);
                        if resp.ok && let Some(client) = client_opt {
                            if let Ok(tree) = client.fetch_widget_summary_tree(&tree_group).await { let _ = tx.try_send(BackendEvent::WidgetTreeLoaded(tree)); }
                            client.dispose_inspector_group(&prev_tree_group).await;
                        }
                    }
                    Err(err) => send_toast(&tx, format!("{label} failed: {err}"), true),
                }
            });
        }
        BackendCommand::ToggleOverlay(overlay) => {
            if let Some(session) = state.active_session.clone() {
                let tx = event_tx.clone();
                let mut current_overlays = state.overlays.clone();
                tokio::spawn(async move {
                    match send_ipc_to_session(&session, &IpcRequest::ToggleOverlay { overlay }).await {
                        Ok(resp) => {
                            if resp.ok {
                                current_overlays.sync_from_status_message(overlay, &resp.message);
                                let _ = tx.try_send(BackendEvent::OverlayStateUpdated(current_overlays));
                            }
                            send_toast(&tx, resp.message, !resp.ok);
                        }
                        Err(err) => send_toast(&tx, format!("Overlay toggle failed: {err}"), true),
                    }
                });
            } else if let Some(client) = state.vm_client.clone() {
                let (method, params, status_label) = state.overlays.preview_toggle(overlay);
                if client.call_isolate(method, params).await.is_ok() {
                    let _ = state.overlays.toggle(overlay);
                    let _ = event_tx.try_send(BackendEvent::OverlayStateUpdated(state.overlays.clone()));
                    send_toast(event_tx, status_label, false);
                }
            }
        }
        BackendCommand::TakeScreenshot => {
            let Some(session) = state.active_session.clone() else {
                send_toast(event_tx, "No active session to capture screenshot", true);
                return;
            };
            let tx = event_tx.clone();
            tokio::spawn(async move {
                match send_ipc_to_session(&session, &IpcRequest::Screenshot { output_path: None }).await {
                    Ok(resp) => send_toast(&tx, resp.message, !resp.ok),
                    Err(err) => send_toast(&tx, format!("Screenshot failed: {err}"), true),
                }
            });
        }
        BackendCommand::OpenWebDevTools { page } => {
            let session_opt = state.active_session.clone();
            let tx = event_tx.clone();
            tokio::spawn(async move {
                if let Some(session) = session_opt {
                    match send_ipc_to_session(&session, &IpcRequest::OpenDevTools { page: page.clone() }).await {
                        Ok(resp) => send_toast(&tx, resp.message, !resp.ok),
                        Err(_) => spawn_ctl_web_devtools(Some(Path::new(&session.project_root)), page.as_deref()),
                    }
                } else {
                    spawn_ctl_web_devtools(None, page.as_deref());
                }
            });
        }
        BackendCommand::SetNetworkRecording(enabled) => {
            state.network_recording = enabled;
            if let Some(c) = state.vm_client.clone() { tokio::spawn(async move { c.set_http_timeline_logging(enabled).await }); }
        }
        BackendCommand::ClearNetworkProfile => {
            state.http_updated_since = None;
            if let Some(c) = state.vm_client.clone() { tokio::spawn(async move { c.clear_http_profile().await }); }
        }
        BackendCommand::FetchHttpRequestDetail { request_id } => {
            if let Some(c) = state.vm_client.clone() {
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    if let Ok(detail) = c.get_http_profile_request(&request_id).await {
                        let _ = tx.try_send(BackendEvent::HttpRequestDetailLoaded { request_id, detail });
                    }
                });
            }
        }
        BackendCommand::RefreshWidgetTree => {
            if let Some(c) = state.vm_client.clone() {
                let (tree_group, prev_group) = state.next_tree_group();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    match c.fetch_widget_summary_tree(&tree_group).await {
                        Ok(tree) => { let _ = tx.try_send(BackendEvent::WidgetTreeLoaded(tree)); }
                        Err(err) => send_toast(&tx, format!("Inspector refresh failed: {err}"), true),
                    }
                    c.dispose_inspector_group(&prev_group).await;
                });
            }
        }
        BackendCommand::SelectWidgetNode { value_id } => {
            if let Some(c) = state.vm_client.clone() {
                let tree_group = state.active_tree_group.clone();
                let (props_group, prev_props_group) = state.next_props_group();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    c.set_widget_selection_by_id(&value_id, &tree_group).await;
                    if let Ok(properties) = c.fetch_widget_properties(&value_id, &props_group).await {
                        let _ = tx.try_send(BackendEvent::WidgetPropertiesLoaded { value_id, properties });
                    }
                    c.dispose_inspector_group(&prev_props_group).await;
                });
            }
        }
        BackendCommand::ExpandWidgetChildren { value_id } => {
            if let Some(c) = state.vm_client.clone() {
                let tree_group = state.active_tree_group.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    if let Ok(children) = c.fetch_widget_children(&value_id, &tree_group).await && !children.is_empty() {
                        let _ = tx.try_send(BackendEvent::WidgetChildrenLoaded { parent_value_id: value_id, children });
                    }
                });
            }
        }
        BackendCommand::SetMemoryPolling(enabled) => state.memory_polling = enabled,
        BackendCommand::ForceGc => {
            if let Some(c) = state.vm_client.clone() {
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    match c.trigger_force_gc().await {
                        Ok(snap) => { let _ = tx.try_send(BackendEvent::MemoryAppended(snap)); send_toast(&tx, "Garbage Collection completed", false); }
                        Err(err) => send_toast(&tx, err.to_string(), true),
                    }
                });
            }
        }
        BackendCommand::OpenInZed(loc) => {
            let root = state.active_session.as_ref().map(|s| PathBuf::from(&s.project_root)).or_else(|| state.preferred_project.as_ref().map(PathBuf::from));
            match open_source_in_zed(&loc, root.as_deref()) {
                Ok(msg) => send_toast(event_tx, msg, false),
                Err(err) => send_toast(event_tx, err.to_string(), true),
            }
        }
        BackendCommand::Shutdown => {}
    }
}

async fn handle_vm_push_event(ev: VmPushEvent, state: &mut WorkerState, event_tx: &Sender<BackendEvent>) {
    match ev {
        VmPushEvent::Frame(frame) => { let _ = event_tx.try_send(BackendEvent::FrameAppended(frame)); }
        VmPushEvent::Log(entry) => { let _ = event_tx.try_send(BackendEvent::LogAppended(entry)); }
        VmPushEvent::LogWithTruncatedRefs(parsed) => {
            if let Some(client) = state.vm_client.clone() {
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    if let Some(updated) = resolve_truncated_log(&client, parsed).await {
                        let _ = tx.try_send(BackendEvent::LogUpdated(updated));
                    }
                });
            }
        }
        VmPushEvent::ServiceExtensionStateChanged { extension, value } => {
            state.overlays.sync_from_extension_event(&extension, &value);
            let _ = event_tx.try_send(BackendEvent::OverlayStateUpdated(state.overlays.clone()));
        }
        VmPushEvent::ServiceExtensionAdded { rpc } => {
            if let Some(ref client) = state.vm_client {
                client.register_extension_rpc(rpc.clone()).await;
                if (rpc == "ext.flutter.inspector.getRootWidgetTree" || rpc == "ext.flutter.inspector.getRootWidgetSummaryTree")
                    && let Some(c) = state.vm_client.clone()
                {
                    let (tree_group, prev_group) = state.next_tree_group();
                    let tx = event_tx.clone();
                    tokio::spawn(async move {
                        if let Ok(tree) = c.fetch_widget_summary_tree(&tree_group).await {
                            let _ = tx.try_send(BackendEvent::WidgetTreeLoaded(tree));
                        }
                        c.dispose_inspector_group(&prev_group).await;
                    });
                }
            }
        }
        VmPushEvent::WidgetInspectEvent => {
            if let Some(c) = state.vm_client.clone() {
                let tree_group = state.active_tree_group.clone();
                let (props_group, prev_props_group) = state.next_props_group();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    if let Ok(selected_node) = c.get_selected_summary_widget(&tree_group).await {
                        let properties = c.fetch_widget_properties(&selected_node.value_id, &props_group).await.unwrap_or_default();
                        let _ = tx.try_send(BackendEvent::WidgetSelectedFromDevice { selected_node, properties });
                    }
                    c.dispose_inspector_group(&prev_props_group).await;
                });
            }
        }
        VmPushEvent::IsolateReloaded => {
            if let Some(c) = state.vm_client.clone() {
                let (tree_group, prev_group) = state.next_tree_group();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let _ = c.refresh_main_isolate().await;
                    if let Ok(tree) = c.fetch_widget_summary_tree(&tree_group).await {
                        let _ = tx.try_send(BackendEvent::WidgetTreeLoaded(tree));
                    }
                    c.dispose_inspector_group(&prev_group).await;
                });
            }
        }
        VmPushEvent::GcNotification => {
            let now = Instant::now();
            if state.last_gc_poll.is_some_and(|prev| now.duration_since(prev) < Duration::from_millis(250)) { return; }
            state.last_gc_poll = Some(now);
            if let Some(c) = state.vm_client.clone() {
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    if let Ok(snap) = c.poll_memory_usage(true).await { let _ = tx.try_send(BackendEvent::MemoryAppended(snap)); }
                });
            }
        }
        VmPushEvent::Disconnected(reason) => {
            state.vm_client = None;
            state.connected_vm_uri = None;
            let _ = event_tx.try_send(BackendEvent::ConnectionChanged(ConnectionStatus::Disconnected(reason)));
        }
    }
}

fn send_toast(tx: &Sender<BackendEvent>, message: impl Into<String>, is_error: bool) {
    let _ = tx.try_send(BackendEvent::Toast { message: message.into(), is_error });
}

async fn resolve_truncated_log(client: &DartVmClient, parsed: ParsedLogRecord) -> Option<LogEntry> {
    let mut message = None;
    let mut error = None;
    let mut stack = parsed.entry.stack_trace.clone();
    if let Some(ref obj_id) = parsed.truncated.message_object_id { message = client.fetch_full_string_object(obj_id).await; }
    if let Some(ref obj_id) = parsed.truncated.error_object_id { error = client.fetch_full_string_object(obj_id).await; }
    if let Some(ref obj_id) = parsed.truncated.stack_object_id && let Some(full_st) = client.fetch_full_string_object(obj_id).await {
        stack = Some(full_st);
    }
    if message.is_none() && error.is_none() && stack == parsed.entry.stack_trace { return None; }
    let base_msg = message.unwrap_or_else(|| parsed.entry.full_text.clone());
    let full_text = match error {
        Some(err) if !err.is_empty() && err != "null" => format!("{base_msg}\nError: {err}"),
        _ => base_msg,
    };
    Some(LogEntry::from_text_with_time(parsed.entry.seq, parsed.entry.timestamp_hms, parsed.entry.kind, &parsed.entry.logger_name, full_text, stack))
}

fn spawn_ctl_web_devtools(project_root: Option<&Path>, page: Option<&str>) {
    let server_bin = dirs::home_dir().map(|h| h.join(".local/bin/flutter-debugger-server")).filter(|p| p.exists()).unwrap_or_else(|| PathBuf::from("flutter-debugger-server"));
    let mut cmd = Command::new(server_bin);
    cmd.arg("ctl").arg("devtools");
    if let Some(root) = project_root { cmd.arg("--project").arg(root); }
    if let Some(p) = page { cmd.arg("--page").arg(p); }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let _ = cmd.spawn();
}
