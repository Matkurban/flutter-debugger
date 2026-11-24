pub mod framing;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::Child;
use tokio::sync::{Mutex, Notify, mpsc};

use crate::cache::{DiskCache, normalize_project_key, now_unix};
use crate::dap::framing::{
    ProxySessionState, build_proxy_response, build_synthetic_initialize_response,
    inspect_adapter_event, is_adapter_termination_event, read_dap_message,
    resolve_and_enrich_launch_or_attach, write_dap_message,
};
use crate::devtools::resolve_or_start_devtools_url;
use crate::ipc::{
    ActiveSessionRecord, IpcRequest, IpcResponse, ServiceOverlay, generate_session_id,
    remove_session_record_by_id, write_session_record,
};
use crate::process_util::{build_command, terminate_child_tree};
use crate::vm_service::VmServiceClient;

/// Runs the enhanced DAP proxy between Zed (stdio) and `flutter`/`dart debug_adapter`.
pub async fn run_dap_proxy() -> Result<(), Box<dyn std::error::Error>> {
    let cache = DiskCache::default_location();
    let default_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let custom_sdk = std::env::var("FLUTTER_SDK_PATH").ok();

    let session_id = generate_session_id();
    let state = Arc::new(Mutex::new(ProxySessionState::new(session_id.clone())));
    let zed_stdout = Arc::new(Mutex::new(tokio::io::stdout()));

    let ipc_listener = TcpListener::bind("127.0.0.1:0").await?;
    let ipc_port = ipc_listener.local_addr()?.port();

    let session_guard = SessionCleanupGuard {
        cache: cache.clone(),
        session_id: session_id.clone(),
    };

    let (to_adapter_tx, to_adapter_rx) = mpsc::channel::<Value>(128);
    let child_handle: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let stop_notify = Arc::new(Notify::new());

    let ipc_state = Arc::clone(&state);
    let ipc_tx = to_adapter_tx.clone();
    let ipc_cache = cache.clone();
    let ipc_child = Arc::clone(&child_handle);
    let ipc_sdk = custom_sdk.clone();
    let ipc_stdout = Arc::clone(&zed_stdout);
    let ipc_stop = Arc::clone(&stop_notify);
    let ipc_handle = tokio::spawn(async move {
        while let Ok((stream, _)) = ipc_listener.accept().await {
            let state_ref = Arc::clone(&ipc_state);
            let tx = ipc_tx.clone();
            let cache_ref = ipc_cache.clone();
            let child_ref = Arc::clone(&ipc_child);
            let sdk_ref = ipc_sdk.clone();
            let stdout_ref = Arc::clone(&ipc_stdout);
            let stop_ref = Arc::clone(&ipc_stop);
            tokio::spawn(async move {
                handle_ipc_connection(
                    stream,
                    state_ref,
                    tx,
                    cache_ref,
                    child_ref,
                    sdk_ref,
                    stdout_ref,
                    stop_ref,
                )
                .await;
            });
        }
    });

    let mut zed_stdin = BufReader::new(tokio::io::stdin());
    let mut saved_initialize: Option<Value> = None;
    let mut pre_launch_queue: Vec<Value> = Vec::new();
    let mut child_spawned = false;
    let mut writer_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut reader_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut to_adapter_rx_opt = Some(to_adapter_rx);

    loop {
        let next_msg = tokio::select! {
            _ = stop_notify.notified() => break,
            res = read_dap_message(&mut zed_stdin) => res,
        };
        let Ok(Some(mut msg)) = next_msg else {
            break;
        };
        let msg_type = msg.get("type").and_then(Value::as_str).unwrap_or_default();
        let command = msg
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let req_seq = msg.get("seq").and_then(Value::as_i64).unwrap_or(1);

        if !child_spawned {
            if msg_type == "request" && command == "initialize" {
                saved_initialize = Some(msg);
                let proxy_seq = state.lock().await.allocate_proxy_seq();
                let synthetic = build_synthetic_initialize_response(proxy_seq, req_seq);
                let mut out = zed_stdout.lock().await;
                if write_dap_message(&mut *out, &synthetic).await.is_err() {
                    break;
                }
                continue;
            }

            if msg_type == "request" && (command == "disconnect" || command == "terminate") {
                let proxy_seq = state.lock().await.allocate_proxy_seq();
                let resp = build_proxy_response(proxy_seq, req_seq, &command, true, None);
                let mut out = zed_stdout.lock().await;
                let _ = write_dap_message(&mut *out, &resp).await;
                break;
            }

            if msg_type != "request" || (command != "launch" && command != "attach") {
                state.lock().await.sync_from_zed_request(&msg);
                pre_launch_queue.push(msg);
                continue;
            }

            let resolved = resolve_and_enrich_launch_or_attach(
                &mut msg,
                &cache,
                &default_cwd,
                custom_sdk.as_deref(),
            );
            state.lock().await.sync_from_zed_request(&msg);

            let (proj_root, executable, is_test, mode, device_id, device_notice) = match resolved {
                Some(r) => (
                    r.project_root,
                    r.executable,
                    r.is_test,
                    r.mode,
                    r.device_id,
                    r.device_notice,
                ),
                None => (
                    default_cwd.clone(),
                    crate::project::platform_cli_filename("flutter").to_string(),
                    false,
                    "debug".to_string(),
                    None,
                    None,
                ),
            };

            let mut child = match spawn_debug_adapter(&executable, &proj_root, is_test) {
                Ok(c) => c,
                Err(err) => {
                    let proxy_seq = state.lock().await.allocate_proxy_seq();
                    let err_msg = format!(
                        "Failed to start `{executable} debug_adapter` in `{}`: {err}",
                        proj_root.display()
                    );
                    let resp =
                        build_proxy_response(proxy_seq, req_seq, &command, false, Some(&err_msg));
                    let mut out = zed_stdout.lock().await;
                    let _ = write_dap_message(&mut *out, &resp).await;
                    break;
                }
            };

            let child_stdin = child
                .stdin
                .take()
                .ok_or("Failed to capture child debug_adapter stdin")?;
            let child_stdout = child
                .stdout
                .take()
                .ok_or("Failed to capture child debug_adapter stdout")?;
            *child_handle.lock().await = Some(child);

            {
                let mut guard = state.lock().await;
                guard.project_root = Some(proj_root.clone());
                guard.mode = mode;
                guard.device_id = device_id;
                guard.is_test = is_test;
            }
            persist_session(&cache, ipc_port, &state).await;

            if let Some(mut rx) = to_adapter_rx_opt.take() {
                writer_task = Some(tokio::spawn(async move {
                    let mut writer = child_stdin;
                    while let Some(m) = rx.recv().await {
                        if write_dap_message(&mut writer, &m).await.is_err() {
                            break;
                        }
                    }
                }));
            }

            let out_state = Arc::clone(&state);
            let out_cache = cache.clone();
            let cache_for_reader = cache.clone();
            let session_id_for_reader = session_id.clone();
            let stop_notify_for_reader = Arc::clone(&stop_notify);
            let out_stdout = Arc::clone(&zed_stdout);
            let out_sdk = custom_sdk.clone();
            reader_task = Some(tokio::spawn(async move {
                let mut reader = BufReader::new(child_stdout);
                let mut profile_devtools_opened = false;

                while let Ok(Some(mut child_msg)) = read_dap_message(&mut reader).await {
                    let mut suppress = false;
                    if child_msg.get("type").and_then(Value::as_str) == Some("response")
                        && let Some(rseq) = child_msg.get("request_seq").and_then(Value::as_i64)
                    {
                        let mut guard = out_state.lock().await;
                        if let Some(tx) = guard.pending_injected.remove(&rseq) {
                            let _ = tx.send(child_msg.clone());
                            suppress = true;
                        } else if guard.suppressed_seqs.remove(&rseq) {
                            suppress = true;
                        }
                    }

                    let is_terminating = is_adapter_termination_event(&child_msg);
                    let (updated, profile_info, current_project_root) = {
                        let mut guard = out_state.lock().await;
                        if is_terminating {
                            guard.terminated = true;
                        }
                        let changed = inspect_adapter_event(&child_msg, &mut guard);
                        let proj_root = guard.project_root.clone();
                        let info = if !guard.terminated
                            && !profile_devtools_opened
                            && guard.mode == "profile"
                            && (guard.devtools_uri.is_some() || guard.vm_service_uri.is_some())
                        {
                            Some((
                                proj_root
                                    .clone()
                                    .unwrap_or_else(|| PathBuf::from(".")),
                                guard.devtools_uri.clone(),
                                guard.vm_service_uri.clone(),
                            ))
                        } else {
                            None
                        };
                        (changed && !guard.terminated, info, proj_root)
                    };

                    if is_terminating {
                        remove_session_record_by_id(&out_cache, &session_id_for_reader);
                    } else if updated {
                        persist_session(&out_cache, ipc_port, &out_state).await;
                    }
                    if let Some((root, dt, vm)) = profile_info
                        && let Some(url) = resolve_or_start_devtools_url(
                            &out_cache,
                            &root,
                            dt.as_deref(),
                            vm.as_deref(),
                            Some("performance"),
                            out_sdk.as_deref(),
                        )
                        .await
                    {
                        profile_devtools_opened = true;
                        open_url_in_browser(&url);
                    }

                    if !suppress {
                        if let Some(ref root) = current_project_root {
                            rewrite_dap_output_event(&mut child_msg, root);
                        }
                        let mut out = out_stdout.lock().await;
                        if write_dap_message(&mut *out, &child_msg).await.is_err() {
                            break;
                        }
                    }
                }

                remove_session_record_by_id(&cache_for_reader, &session_id_for_reader);
                stop_notify_for_reader.notify_one();
            }));

            if let Some(init_msg) = saved_initialize.take() {
                let init_seq = init_msg.get("seq").and_then(Value::as_i64).unwrap_or(1);
                let init_rx = state.lock().await.register_suppressed_waiter(init_seq);
                if to_adapter_tx.send(init_msg).await.is_err() {
                    break;
                }
                let _ = tokio::time::timeout(Duration::from_secs(5), init_rx).await;
            }

            child_spawned = true;

            if let Some(notice) = device_notice {
                let proxy_seq = state.lock().await.allocate_proxy_seq();
                let raw_notice = format!("[flutter-debugger] {notice}\n");
                let colored_notice =
                    flutter_debugger_protocol::colorize_dap_output(&raw_notice, Some("console"));
                let output_event = json!({
                    "seq": proxy_seq,
                    "type": "event",
                    "event": "output",
                    "body": {
                        "category": "console",
                        "output": colored_notice
                    }
                });
                let mut out = zed_stdout.lock().await;
                let _ = write_dap_message(&mut *out, &output_event).await;
            }

            if to_adapter_tx.send(msg).await.is_err() {
                break;
            }
            for queued in pre_launch_queue.drain(..) {
                if to_adapter_tx.send(queued).await.is_err() {
                    break;
                }
            }
            continue;
        }

        if msg_type == "request" && (command == "disconnect" || command == "terminate") {
            state.lock().await.terminated = true;
            remove_session_record_by_id(&cache, &session_id);
        }
        state.lock().await.sync_from_zed_request(&msg);
        if to_adapter_tx.send(msg).await.is_err() {
            break;
        }
    }

    drop(to_adapter_tx);
    if let Some(mut child) = child_handle.lock().await.take() {
        terminate_child_tree(&mut child).await;
    }
    ipc_handle.abort();
    if let Some(h) = writer_task {
        let _ = h.await;
    }
    if let Some(h) = reader_task {
        let _ = h.await;
    }
    drop(session_guard);

    Ok(())
}

fn spawn_debug_adapter(executable: &str, cwd: &Path, is_test: bool) -> std::io::Result<Child> {
    let mut args = vec!["debug_adapter"];
    if is_test {
        args.push("--test");
    }

    build_command(executable, &args, cwd)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
}

async fn inject_and_await(
    state: &Arc<Mutex<ProxySessionState>>,
    to_adapter_tx: &mpsc::Sender<Value>,
    command: &str,
    arguments: Value,
    timeout_secs: u64,
) -> Result<(Value, u64), String> {
    let start = Instant::now();
    let (seq, rx) = {
        let mut guard = state.lock().await;
        guard.allocate_injected_request()
    };

    let req = json!({
        "seq": seq,
        "type": "request",
        "command": command,
        "arguments": arguments
    });

    if to_adapter_tx.send(req).await.is_err() {
        state.lock().await.pending_injected.remove(&seq);
        return Err("Debug adapter session is closed".to_string());
    }

    match tokio::time::timeout(Duration::from_secs(timeout_secs), rx).await {
        Ok(Ok(resp)) => {
            let elapsed = start.elapsed().as_millis() as u64;
            let ok = resp.get("success").and_then(Value::as_bool).unwrap_or(true);
            if ok {
                Ok((resp, elapsed))
            } else {
                let err_msg = resp
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Adapter request failed");
                Err(err_msg.to_string())
            }
        }
        Ok(Err(_)) => Err("Debug adapter closed before responding".to_string()),
        Err(_) => {
            state.lock().await.pending_injected.remove(&seq);
            Err(format!("Timed out waiting for `{command}` response"))
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_ipc_connection(
    mut stream: tokio::net::TcpStream,
    state: Arc<Mutex<ProxySessionState>>,
    to_adapter_tx: mpsc::Sender<Value>,
    cache: DiskCache,
    child_handle: Arc<Mutex<Option<Child>>>,
    custom_sdk: Option<String>,
    zed_stdout: Arc<Mutex<tokio::io::Stdout>>,
    stop_notify: Arc<Notify>,
) {
    let (read_half, mut write_half) = stream.split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }

    let response = match serde_json::from_str::<IpcRequest>(line.trim()) {
        Ok(IpcRequest::HotReload) => {
            let (mode, dev, vm, dt) = snapshot_meta(&state).await;
            if mode != "debug" {
                IpcResponse {
                    ok: false,
                    message: format!("Hot Reload is disabled in `{mode}` mode"),
                    duration_ms: None,
                    mode: Some(mode),
                    device_id: dev,
                    vm_service_uri: vm,
                    devtools_uri: dt,
                    ..Default::default()
                }
            } else {
                match inject_and_await(
                    &state,
                    &to_adapter_tx,
                    "hotReload",
                    json!({ "reason": "save" }),
                    12,
                )
                .await
                {
                    Ok((_, ms)) => IpcResponse {
                        ok: true,
                        message: format!("Hot Reload: {ms}ms"),
                        duration_ms: Some(ms),
                        mode: Some(mode),
                        device_id: dev,
                        vm_service_uri: vm,
                        devtools_uri: dt,
                        ..Default::default()
                    },
                    Err(err) => IpcResponse {
                        ok: false,
                        message: format!("Hot Reload failed: {err}"),
                        duration_ms: None,
                        mode: Some(mode),
                        device_id: dev,
                        vm_service_uri: vm,
                        devtools_uri: dt,
                        ..Default::default()
                    },
                }
            }
        }
        Ok(IpcRequest::HotRestart) => {
            let (mode, dev, vm, dt) = snapshot_meta(&state).await;
            if mode != "debug" {
                IpcResponse {
                    ok: false,
                    message: format!("Hot Restart is disabled in `{mode}` mode"),
                    duration_ms: None,
                    mode: Some(mode),
                    device_id: dev,
                    vm_service_uri: vm,
                    devtools_uri: dt,
                    ..Default::default()
                }
            } else {
                match inject_and_await(
                    &state,
                    &to_adapter_tx,
                    "hotRestart",
                    json!({ "reason": "manual" }),
                    15,
                )
                .await
                {
                    Ok((_, ms)) => IpcResponse {
                        ok: true,
                        message: format!("Hot Restart: {ms}ms"),
                        duration_ms: Some(ms),
                        mode: Some(mode),
                        device_id: dev,
                        vm_service_uri: vm,
                        devtools_uri: dt,
                        ..Default::default()
                    },
                    Err(err) => IpcResponse {
                        ok: false,
                        message: format!("Hot Restart failed: {err}"),
                        duration_ms: None,
                        mode: Some(mode),
                        device_id: dev,
                        vm_service_uri: vm,
                        devtools_uri: dt,
                        ..Default::default()
                    },
                }
            }
        }
        Ok(IpcRequest::Stop) => {
            let (mode, dev, vm, dt) = snapshot_meta(&state).await;
            let session_id = {
                let mut g = state.lock().await;
                g.terminated = true;
                g.session_id.clone()
            };
            // Remove disk session record FIRST so any immediate CodeLens refresh sees no active session.
            remove_session_record_by_id(&cache, &session_id);
            let resp = IpcResponse {
                ok: true,
                message: "Stopped Flutter debug session".to_string(),
                duration_ms: None,
                mode: Some(mode),
                device_id: dev,
                vm_service_uri: vm,
                devtools_uri: dt,
                ..Default::default()
            };
            if let Ok(mut payload) = serde_json::to_string(&resp) {
                payload.push('\n');
                let _ = write_half.write_all(payload.as_bytes()).await;
                let _ = write_half.flush().await;
            }
            let _ = inject_and_await(&state, &to_adapter_tx, "terminate", json!({}), 3).await;
            if let Some(mut child) = child_handle.lock().await.take() {
                terminate_child_tree(&mut child).await;
            }
            let terminated_seq = state.lock().await.allocate_proxy_seq();
            let terminated_event = json!({
                "seq": terminated_seq,
                "type": "event",
                "event": "terminated"
            });
            {
                let mut out = zed_stdout.lock().await;
                let _ = write_dap_message(&mut *out, &terminated_event).await;
            }
            remove_session_record_by_id(&cache, &session_id);
            stop_notify.notify_one();
            return;
        }
        Ok(IpcRequest::ToggleOverlay { overlay }) => {
            handle_overlay_toggle(&state, &to_adapter_tx, overlay).await
        }
        Ok(IpcRequest::CallServiceExtension { method, params }) => {
            let (mode, dev, vm, dt, params_with_isolate) = {
                let guard = state.lock().await;
                (
                    guard.mode.clone(),
                    guard.device_id.clone(),
                    guard.vm_service_uri.clone(),
                    guard.devtools_uri.clone(),
                    attach_isolate_id(&params, &guard, &method),
                )
            };
            let dap_res = inject_and_await(
                &state,
                &to_adapter_tx,
                "callService",
                json!({ "method": method, "params": params_with_isolate }),
                10,
            )
            .await;
            match dap_res {
                Ok((_, ms)) => IpcResponse {
                    ok: true,
                    message: format!("Called `{method}` ({ms}ms)"),
                    duration_ms: Some(ms),
                    mode: Some(mode),
                    device_id: dev,
                    vm_service_uri: vm,
                    devtools_uri: dt,
                    ..Default::default()
                },
                Err(dap_err) => {
                    if let Some(vm_uri) = vm.as_deref()
                        && let Ok(mut client) = VmServiceClient::connect(vm_uri).await
                        && client.call_service_extension(&method, params).await.is_ok()
                    {
                        IpcResponse {
                            ok: true,
                            message: format!("Called `{method}` via VM Service"),
                            duration_ms: None,
                            mode: Some(mode),
                            device_id: dev,
                            vm_service_uri: vm,
                            devtools_uri: dt,
                            ..Default::default()
                        }
                    } else {
                        IpcResponse {
                            ok: false,
                            message: format!("Service extension `{method}` failed: {dap_err}"),
                            duration_ms: None,
                            mode: Some(mode),
                            device_id: dev,
                            vm_service_uri: vm,
                            devtools_uri: dt,
                            ..Default::default()
                        }
                    }
                }
            }
        }
        Ok(IpcRequest::OpenDevTools { page }) => {
            let (mode, dev, vm, dt) = snapshot_meta(&state).await;
            let project_root = state
                .lock()
                .await
                .project_root
                .clone()
                .unwrap_or_else(|| PathBuf::from("."));
            if let Some(url) = resolve_or_start_devtools_url(
                &cache,
                &project_root,
                dt.as_deref(),
                vm.as_deref(),
                page.as_deref(),
                custom_sdk.as_deref(),
            )
            .await
            {
                open_url_in_browser(&url);
                IpcResponse {
                    ok: true,
                    message: format!("Opened DevTools: {url}"),
                    duration_ms: None,
                    mode: Some(mode),
                    device_id: dev,
                    vm_service_uri: vm,
                    devtools_uri: Some(url),
                    ..Default::default()
                }
            } else {
                IpcResponse {
                    ok: false,
                    message: "DevTools URI is not ready yet for this session".to_string(),
                    duration_ms: None,
                    mode: Some(mode),
                    device_id: dev,
                    vm_service_uri: vm,
                    devtools_uri: dt,
                    ..Default::default()
                }
            }
        }
        Ok(IpcRequest::Screenshot { output_path }) => {
            let (mode, dev, vm, dt) = snapshot_meta(&state).await;
            let project_root = state
                .lock()
                .await
                .project_root
                .clone()
                .unwrap_or_else(|| PathBuf::from("."));
            let out_file = output_path.unwrap_or_else(|| {
                project_root
                    .join(format!("flutter_screenshot_{}.png", now_unix()))
                    .to_string_lossy()
                    .into_owned()
            });

            if let Some(vm_uri) = vm.as_deref()
                && let Ok(mut client) = VmServiceClient::connect(vm_uri).await
                && let Ok(png_bytes) = client.capture_screenshot_png().await
                && std::fs::write(&out_file, png_bytes).is_ok()
            {
                IpcResponse {
                    ok: true,
                    message: format!("Screenshot saved to {out_file}"),
                    duration_ms: None,
                    mode: Some(mode),
                    device_id: dev,
                    vm_service_uri: vm,
                    devtools_uri: dt,
                    ..Default::default()
                }
            } else {
                let exe = crate::project::resolve_sdk_executable(
                    &project_root,
                    crate::project::ProjectKind::Flutter,
                    custom_sdk.as_deref(),
                );
                let mut args = vec!["screenshot".to_string(), "-o".to_string(), out_file.clone()];
                if let Some(d) = dev.as_deref() {
                    args.push("-d".to_string());
                    args.push(d.to_string());
                }
                match build_command(&exe, &args, &project_root)
                    .output()
                    .await
                {
                    Ok(out) if out.status.success() => IpcResponse {
                        ok: true,
                        message: format!("Screenshot saved to {out_file}"),
                        duration_ms: None,
                        mode: Some(mode),
                        device_id: dev,
                        vm_service_uri: vm,
                        devtools_uri: dt,
                        ..Default::default()
                    },
                    Ok(out) => IpcResponse {
                        ok: false,
                        message: format!(
                            "Screenshot failed: {}",
                            String::from_utf8_lossy(&out.stderr).trim()
                        ),
                        duration_ms: None,
                        mode: Some(mode),
                        device_id: dev,
                        vm_service_uri: vm,
                        devtools_uri: dt,
                        ..Default::default()
                    },
                    Err(err) => IpcResponse::err(format!("Failed to run screenshot: {err}")),
                }
            }
        }
        Ok(IpcRequest::SetExceptionPauseMode { mode: pause_mode }) => {
            let (mode, dev, vm, dt) = snapshot_meta(&state).await;
            let filters = pause_mode.dap_filters();
            match inject_and_await(
                &state,
                &to_adapter_tx,
                "setExceptionBreakpoints",
                json!({ "filters": filters }),
                8,
            )
            .await
            {
                Ok((_, ms)) => {
                    let scope = {
                        let mut g = state.lock().await;
                        g.exception_pause_mode = pause_mode;
                        g.debug_library_scope
                    };
                    IpcResponse {
                        ok: true,
                        message: format!("Exception breakpoints: {}", pause_mode.description()),
                        duration_ms: Some(ms),
                        mode: Some(mode),
                        device_id: dev,
                        vm_service_uri: vm,
                        devtools_uri: dt,
                        exception_pause_mode: Some(pause_mode),
                        debug_library_scope: Some(scope),
                    }
                }
                Err(err) => IpcResponse::err(format!(
                    "Failed to update exception breakpoints: {err}"
                )),
            }
        }
        Ok(IpcRequest::SetDebugLibraryScope { scope }) => {
            let (mode, dev, vm, dt) = snapshot_meta(&state).await;
            let (debug_sdk, debug_pkg) = scope.dap_flags();
            match inject_and_await(
                &state,
                &to_adapter_tx,
                "updateDebugOptions",
                json!({
                    "debugSdkLibraries": debug_sdk,
                    "debugExternalPackageLibraries": debug_pkg
                }),
                8,
            )
            .await
            {
                Ok((_, ms)) => {
                    let pause_mode = {
                        let mut g = state.lock().await;
                        g.debug_library_scope = scope;
                        g.exception_pause_mode
                    };
                    IpcResponse {
                        ok: true,
                        message: format!("Debug scope: {}", scope.description()),
                        duration_ms: Some(ms),
                        mode: Some(mode),
                        device_id: dev,
                        vm_service_uri: vm,
                        devtools_uri: dt,
                        exception_pause_mode: Some(pause_mode),
                        debug_library_scope: Some(scope),
                    }
                }
                Err(err) => {
                    IpcResponse::err(format!("Failed to update debug library scope: {err}"))
                }
            }
        }
        Ok(IpcRequest::Status) => {
            let (mode, dev, vm, dt, pause_mode, scope) = {
                let g = state.lock().await;
                (
                    g.mode.clone(),
                    g.device_id.clone(),
                    g.vm_service_uri.clone(),
                    g.devtools_uri.clone(),
                    g.exception_pause_mode,
                    g.debug_library_scope,
                )
            };
            IpcResponse {
                ok: true,
                message: "active".to_string(),
                duration_ms: None,
                mode: Some(mode),
                device_id: dev,
                vm_service_uri: vm,
                devtools_uri: dt,
                exception_pause_mode: Some(pause_mode),
                debug_library_scope: Some(scope),
            }
        }
        Err(err) => IpcResponse::err(format!("Invalid IPC request: {err}")),
    };

    if let Ok(mut payload) = serde_json::to_string(&response) {
        payload.push('\n');
        let _ = write_half.write_all(payload.as_bytes()).await;
    }
}

fn attach_isolate_id(params: &Value, state: &ProxySessionState, method: &str) -> Value {
    let mut enriched = params.clone();
    if let Some(obj) = enriched.as_object_mut()
        && !obj.contains_key("isolateId")
        && let Some(isolate_id) = state
            .service_extension_isolates
            .get(method)
            .or(state.main_isolate_id.as_ref())
    {
        obj.insert("isolateId".to_string(), json!(isolate_id));
    }
    enriched
}

async fn handle_overlay_toggle(
    state: &Arc<Mutex<ProxySessionState>>,
    to_adapter_tx: &mpsc::Sender<Value>,
    overlay: ServiceOverlay,
) -> IpcResponse {
    let (mode, dev, vm, dt, method, params, params_with_isolate, status_label) = {
        let guard = state.lock().await;
        if guard.mode == "release" {
            return IpcResponse {
                ok: false,
                message: "Visual overlays are unavailable in release mode".to_string(),
                duration_ms: None,
                mode: Some(guard.mode.clone()),
                device_id: guard.device_id.clone(),
                vm_service_uri: guard.vm_service_uri.clone(),
                devtools_uri: guard.devtools_uri.clone(),
                ..Default::default()
            };
        }
        let (m, p, label) = guard.overlays.preview_toggle(overlay);
        let p_iso = attach_isolate_id(&p, &guard, m);
        (
            guard.mode.clone(),
            guard.device_id.clone(),
            guard.vm_service_uri.clone(),
            guard.devtools_uri.clone(),
            m,
            p,
            p_iso,
            label,
        )
    };

    match inject_and_await(
        state,
        to_adapter_tx,
        "callService",
        json!({ "method": method, "params": params_with_isolate }),
        10,
    )
    .await
    {
        Ok((_, ms)) => {
            state.lock().await.overlays.toggle(overlay);
            IpcResponse {
                ok: true,
                message: status_label,
                duration_ms: Some(ms),
                mode: Some(mode),
                device_id: dev,
                vm_service_uri: vm,
                devtools_uri: dt,
                ..Default::default()
            }
        }
        Err(dap_err) => {
            if let Some(vm_uri) = vm.as_deref()
                && let Ok(mut client) = VmServiceClient::connect(vm_uri).await
                && client.call_service_extension(method, params).await.is_ok()
            {
                state.lock().await.overlays.toggle(overlay);
                return IpcResponse {
                    ok: true,
                    message: status_label,
                    duration_ms: None,
                    mode: Some(mode),
                    device_id: dev,
                    vm_service_uri: vm,
                    devtools_uri: dt,
                    ..Default::default()
                };
            }
            IpcResponse {
                ok: false,
                message: format!("Failed to toggle {}: {dap_err}", overlay.label()),
                duration_ms: None,
                mode: Some(mode),
                device_id: dev,
                vm_service_uri: vm,
                devtools_uri: dt,
                ..Default::default()
            }
        }
    }
}

async fn snapshot_meta(
    state: &Arc<Mutex<ProxySessionState>>,
) -> (String, Option<String>, Option<String>, Option<String>) {
    let guard = state.lock().await;
    (
        guard.mode.clone(),
        guard.device_id.clone(),
        guard.vm_service_uri.clone(),
        guard.devtools_uri.clone(),
    )
}

async fn persist_session(cache: &DiskCache, port: u16, state: &Arc<Mutex<ProxySessionState>>) {
    let guard = state.lock().await;
    if guard.terminated {
        return;
    }
    let Some(root) = &guard.project_root else {
        return;
    };
    let record = ActiveSessionRecord {
        session_id: guard.session_id.clone(),
        project_root: normalize_project_key(root),
        device_id: guard.device_id.clone(),
        mode: guard.mode.clone(),
        pid: std::process::id(),
        ipc_port: port,
        vm_service_uri: guard.vm_service_uri.clone(),
        devtools_uri: guard.devtools_uri.clone(),
        started_at: guard.started_at,
        is_test: guard.is_test,
        kind: "dap".to_string(),
    };
    let _ = write_session_record(cache, &record);
}

struct SessionCleanupGuard {
    cache: DiskCache,
    session_id: String,
}

impl Drop for SessionCleanupGuard {
    fn drop(&mut self) {
        remove_session_record_by_id(&self.cache, &self.session_id);
    }
}


/// Rewrites DAP `output` events so `file://`, `package:`, and bare local `.dart:line(:col)?`
/// locations become clickable `zed://file/...` links in Zed Debug Console, enriches missing
/// `body.source` metadata (keeping `body.source.path` as a pure local path), and applies
/// multi-state ANSI colors.
pub fn rewrite_dap_output_event(msg: &mut Value, project_root: &Path) {
    if msg.get("type").and_then(Value::as_str) != Some("event")
        || msg.get("event").and_then(Value::as_str) != Some("output")
    {
        return;
    }

    let Some(body) = msg.get_mut("body").and_then(Value::as_object_mut) else {
        return;
    };

    // 1. Normalize an existing `body.source.path` if it uses `file://`, `package:`, or `zed://file`
    if let Some(source_obj) = body.get_mut("source").and_then(Value::as_object_mut)
        && let Some(raw_path) = source_obj
            .get("path")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
        && (raw_path.starts_with("file://")
            || raw_path.starts_with("package:")
            || raw_path.starts_with("zed://file"))
        && let Some(resolved) =
            flutter_debugger_protocol::resolve_dart_uri_to_local_path(&raw_path, Some(project_root))
    {
        source_obj.insert("path".to_string(), json!(resolved.to_string_lossy()));
    }

    let Some(raw_output) = body
        .get("output")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
    else {
        return;
    };

    // 2. Rewrite Dart locations to `zed://file/...` and enrich missing `body.source` / `line` / `column`
    let (rewritten, first_loc) =
        flutter_debugger_protocol::rewrite_dart_output_paths(&raw_output, project_root);

    let source_missing = body.get("source").is_none_or(Value::is_null);
    if source_missing
        && let Some((abs_path, line, col_opt)) = first_loc
    {
        let root_clean = flutter_debugger_protocol::clean_relative_segments(project_root);
        let root_canon = std::fs::canonicalize(project_root).ok();
        let abs_canon = std::fs::canonicalize(&abs_path).ok();
        let name = abs_path
            .strip_prefix(&root_clean)
            .ok()
            .or_else(|| abs_path.strip_prefix(project_root).ok())
            .or_else(|| {
                if let (Some(ac), Some(rc)) = (&abs_canon, &root_canon) {
                    ac.strip_prefix(rc).ok()
                } else {
                    None
                }
            })
            .map(|rel| rel.to_string_lossy().replace('\\', "/"))
            .or_else(|| {
                abs_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| abs_path.to_string_lossy().into_owned());

        body.insert(
            "source".to_string(),
            json!({
                "name": name,
                "path": abs_path.to_string_lossy(),
            }),
        );
        if body.get("line").is_none_or(Value::is_null) {
            body.insert("line".to_string(), json!(line));
        }
        if let Some(col) = col_opt
            && body.get("column").is_none_or(Value::is_null)
        {
            body.insert("column".to_string(), json!(col));
        }
    }

    // 3. Colorize output lines by DAP `category` and Flutter log semantics
    let category = body
        .get("category")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let colored =
        flutter_debugger_protocol::colorize_dap_output(&rewritten, category.as_deref());
    if colored != raw_output {
        body.insert("output".to_string(), Value::String(colored));
    }
}

pub fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    #[cfg(windows)]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{list_active_sessions, send_ipc_request};

    #[test]
    fn rewrites_dap_output_events_and_enriches_missing_source_metadata() {
        let dir = tempfile::tempdir().expect("tempdir");
        let proj_root = dir.path().join("my_app");
        let dart_tool = proj_root.join(".dart_tool");
        std::fs::create_dir_all(&dart_tool).expect("mkdir .dart_tool");
        let nicepay_file = proj_root
            .join("lib")
            .join("pages")
            .join("home")
            .join("nicepay_view.dart");
        std::fs::create_dir_all(nicepay_file.parent().expect("parent")).expect("mkdir pages/home");
        std::fs::write(&nicepay_file, "void main() {}\n").expect("write nicepay_view.dart");

        let pub_cache_pkg = dir.path().join("pub_cache").join("provider-6.1.0");
        std::fs::create_dir_all(pub_cache_pkg.join("lib").join("src")).expect("mkdir provider");
        let pub_cache_uri = url::Url::from_directory_path(&pub_cache_pkg)
            .expect("pub_cache file URI")
            .to_string();

        let pkg_cfg = json!({
            "configVersion": 2,
            "packages": [
                { "name": "my_app", "rootUri": "../", "packageUri": "lib/" },
                { "name": "provider", "rootUri": pub_cache_uri, "packageUri": "lib/" }
            ]
        });
        std::fs::write(
            dart_tool.join("package_config.json"),
            serde_json::to_vec_pretty(&pkg_cfg).expect("json"),
        )
        .expect("write package_config.json");

        // 1a. Flutter rendering exception banner on `stderr` -> colored red (`\x1b[31m`)
        let mut msg_err_banner = json!({
            "seq": 9,
            "type": "event",
            "event": "output",
            "body": {
                "category": "stderr",
                "output": "════════ Exception caught by widgets library ════════\nThe following assertion was thrown building NicePayView:\n"
            }
        });
        rewrite_dap_output_event(&mut msg_err_banner, &proj_root);
        assert_eq!(
            msg_err_banner["body"]["output"].as_str().unwrap(),
            "\x1b[31m════════ Exception caught by widgets library ════════\x1b[0m\n\x1b[31mThe following assertion was thrown building NicePayView:\x1b[0m\n"
        );

        // 1b. Flutter error-causing widget detail on `stdout` with bare relative path -> blue (`\x1b[34m`) + clickable `zed://file/...:374:40`
        let expected_nicepay_uri =
            flutter_debugger_protocol::format_zed_file_uri(&nicepay_file, Some(374), Some(40));
        let mut msg1 = json!({
            "seq": 10,
            "type": "event",
            "event": "output",
            "body": {
                "category": "stdout",
                "output": "The relevant error-causing widget was:\n  Swiper Swiper (lib/pages/home/nicepay_view.dart:374:40)\n",
                "source": null
            }
        });
        rewrite_dap_output_event(&mut msg1, &proj_root);
        assert_eq!(
            msg1["body"]["output"].as_str().unwrap(),
            format!(
                "\x1b[34mThe relevant error-causing widget was:\x1b[0m\n\x1b[34m  Swiper Swiper ({expected_nicepay_uri})\x1b[0m\n"
            )
        );
        assert_eq!(
            msg1["body"]["source"]["name"],
            "lib/pages/home/nicepay_view.dart"
        );
        assert_eq!(
            msg1["body"]["source"]["path"],
            nicepay_file.to_string_lossy().as_ref()
        );
        assert!(
            !msg1["body"]["source"]["path"]
                .as_str()
                .unwrap()
                .starts_with("zed://"),
            "body.source.path must remain a pure local filesystem path"
        );
        assert_eq!(msg1["body"]["line"], 374);
        assert_eq!(msg1["body"]["column"], 40);

        // 2. Dart stack trace with ANSI dim escape codes (`\x1b[2m`) and `package:my_app/...` -> `\x1b[90m` + `zed://file/...`
        let home_file = proj_root.join("lib").join("pages").join("home.dart");
        let expected_home_uri =
            flutter_debugger_protocol::format_zed_file_uri(&home_file, Some(42), Some(5));
        let mut msg2 = json!({
            "seq": 11,
            "type": "event",
            "event": "output",
            "body": {
                "category": "stdout",
                "output": "\x1b[2m#0      _HomeState.build (package:my_app/pages/home.dart:42:5)\x1b[0m\n"
            }
        });
        rewrite_dap_output_event(&mut msg2, &proj_root);
        assert_eq!(
            msg2["body"]["output"].as_str().unwrap(),
            format!("\x1b[90m#0      _HomeState.build ({expected_home_uri})\x1b[0m\n")
        );
        assert_eq!(msg2["body"]["source"]["name"], "lib/pages/home.dart");
        assert_eq!(
            msg2["body"]["source"]["path"],
            home_file.to_string_lossy().as_ref()
        );
        assert_eq!(msg2["body"]["line"], 42);
        assert_eq!(msg2["body"]["column"], 5);

        // 3. Third-party pub package `package:provider/...` + existing `file://` source.path normalization
        let provider_file = pub_cache_pkg.join("lib").join("src").join("provider.dart");
        let provider_file_uri = url::Url::from_file_path(&provider_file)
            .expect("provider file URI")
            .to_string();
        let expected_provider_uri =
            flutter_debugger_protocol::format_zed_file_uri(&provider_file, Some(19), Some(7));
        let mut msg3 = json!({
            "seq": 12,
            "type": "event",
            "event": "output",
            "body": {
                "category": "stdout",
                "output": "#1      Provider.of (package:provider/src/provider.dart:19:7) see https://pub.dev/packages/provider\n",
                "source": {
                    "name": "package:provider/src/provider.dart",
                    "path": provider_file_uri
                }
            }
        });
        rewrite_dap_output_event(&mut msg3, &proj_root);
        let out3 = msg3["body"]["output"].as_str().unwrap();
        assert!(
            out3.contains(&format!("({expected_provider_uri})")),
            "Actual out3: {out3}"
        );
        assert!(out3.contains("https://pub.dev/packages/provider"));
        assert_eq!(
            msg3["body"]["source"]["path"],
            provider_file.to_string_lossy().as_ref()
        );
    }

    #[tokio::test]
    async fn stop_ipc_removes_session_record_before_returning_ok_response() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::with_root(dir.path().join(".cache"));
        let proj_root = dir.path().join("app");
        std::fs::create_dir_all(&proj_root).expect("mkdir");

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();

        let session_id = "test-stop-immediate-1".to_string();
        let state = Arc::new(Mutex::new(ProxySessionState::new(session_id.clone())));
        {
            let mut g = state.lock().await;
            g.project_root = Some(proj_root.clone());
            g.mode = "debug".to_string();
        }
        persist_session(&cache, port, &state).await;
        assert_eq!(list_active_sessions(&cache).len(), 1);

        // Keep `to_adapter_rx` open WITHOUT responding to `"terminate"`, so `inject_and_await`
        // after `write_half` will wait up to 3s — proving `list_active_sessions` is already empty
        // the instant `send_ipc_request` receives `IpcResponse { ok: true }`!
        let (to_adapter_tx, _to_adapter_rx) = mpsc::channel::<Value>(16);
        let child_handle: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
        let zed_stdout = Arc::new(Mutex::new(tokio::io::stdout()));
        let stop_notify = Arc::new(Notify::new());

        let state_srv = Arc::clone(&state);
        let cache_srv = cache.clone();
        let server_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_ipc_connection(
                stream,
                state_srv,
                to_adapter_tx,
                cache_srv,
                child_handle,
                None,
                zed_stdout,
                stop_notify,
            )
            .await;
        });

        let resp = send_ipc_request(&cache, &proj_root, &IpcRequest::Stop)
            .await
            .expect("stop response");
        assert!(resp.ok);
        // At the exact instant the client receives `ok: true`, the session record MUST already be gone.
        assert!(
            list_active_sessions(&cache).is_empty(),
            "Session record must be removed before Stop IPC response returns"
        );
        server_task.abort();
    }

    #[tokio::test]
    async fn terminated_session_removes_record_and_prevents_resurrection_on_trailing_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::with_root(dir.path().join(".cache"));
        let proj_root = dir.path().join("app");
        std::fs::create_dir_all(&proj_root).expect("mkdir");

        let session_id = "test-terminated-no-resurrect".to_string();
        let state = Arc::new(Mutex::new(ProxySessionState::new(session_id.clone())));
        {
            let mut g = state.lock().await;
            g.project_root = Some(proj_root);
            g.mode = "debug".to_string();
        }

        persist_session(&cache, 54321, &state).await;
        assert_eq!(list_active_sessions(&cache).len(), 1);

        // Simulate receiving `terminated` / `flutter.appStop` from adapter when user closes app window
        {
            let mut g = state.lock().await;
            g.terminated = true;
        }
        remove_session_record_by_id(&cache, &session_id);
        assert!(list_active_sessions(&cache).is_empty());

        // Simulate a trailing event (e.g. `output` with DevTools URL or `dart.debuggerUris`)
        // trying to call `persist_session` after termination
        {
            let mut g = state.lock().await;
            g.devtools_uri = Some("http://127.0.0.1:9100".to_string());
        }
        persist_session(&cache, 54321, &state).await;
        assert!(
            list_active_sessions(&cache).is_empty(),
            "Terminated session must never be written back to disk"
        );
    }
}
