pub mod framing;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::{Mutex, mpsc};

use crate::cache::{DiskCache, normalize_project_key};
use crate::dap::framing::{
    ProxySessionState, enrich_launch_or_attach, inspect_adapter_event, read_dap_message,
    write_dap_message,
};
use crate::ipc::{
    ActiveSessionRecord, IpcRequest, IpcResponse, remove_session_record, write_session_record,
};
use crate::project::{ProjectKind, find_project_for_path, resolve_sdk_executable};

/// Runs the transparent DAP proxy between Zed (stdio) and `flutter`/`dart debug_adapter`.
pub async fn run_dap_proxy() -> Result<(), Box<dyn std::error::Error>> {
    let cache = DiskCache::default_location();
    let default_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let project_info = find_project_for_path(&default_cwd);
    let project_root = project_info
        .as_ref()
        .map(|p| p.root.clone())
        .unwrap_or_else(|| default_cwd.clone());
    let project_kind = project_info
        .as_ref()
        .map(|p| p.kind)
        .unwrap_or(ProjectKind::Flutter);

    let custom_sdk = std::env::var("FLUTTER_SDK_PATH").ok();
    let executable = resolve_sdk_executable(&project_root, project_kind, custom_sdk.as_deref());

    let mut child = spawn_debug_adapter(&executable, &project_root)?;
    let child_stdin = child
        .stdin
        .take()
        .ok_or("Failed to capture child debug_adapter stdin")?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or("Failed to capture child debug_adapter stdout")?;

    let state = Arc::new(Mutex::new(ProxySessionState::new()));
    {
        let mut guard = state.lock().await;
        guard.project_root = Some(project_root.clone());
    }

    let ipc_listener = TcpListener::bind("127.0.0.1:0").await?;
    let ipc_port = ipc_listener.local_addr()?.port();

    let session_guard = SessionCleanupGuard {
        cache: cache.clone(),
        project_root: project_root.clone(),
    };
    persist_session(&cache, &project_root, ipc_port, &state).await;

    let (to_adapter_tx, mut to_adapter_rx) = mpsc::channel::<Value>(64);

    // Task 1: Forward queued JSON frames to child debug_adapter stdin
    let adapter_writer_handle = tokio::spawn(async move {
        let mut writer = child_stdin;
        while let Some(msg) = to_adapter_rx.recv().await {
            if write_dap_message(&mut writer, &msg).await.is_err() {
                break;
            }
        }
    });

    // Task 2: Listen for local IPC commands from the LSP server (Hot Reload / Hot Restart / Status)
    let ipc_state = Arc::clone(&state);
    let ipc_tx = to_adapter_tx.clone();
    let ipc_handle = tokio::spawn(async move {
        while let Ok((stream, _)) = ipc_listener.accept().await {
            let state_ref = Arc::clone(&ipc_state);
            let tx = ipc_tx.clone();
            tokio::spawn(async move {
                handle_ipc_connection(stream, state_ref, tx).await;
            });
        }
    });

    // Task 3: Read from child debug_adapter stdout -> Zed stdout (filtering injected responses)
    let out_state = Arc::clone(&state);
    let out_cache = cache.clone();
    let out_root = project_root.clone();
    let adapter_reader_handle = tokio::spawn(async move {
        let mut reader = BufReader::new(child_stdout);
        let mut zed_stdout = tokio::io::stdout();
        let mut profile_devtools_opened = false;

        while let Ok(Some(msg)) = read_dap_message(&mut reader).await {
            let suppress = if msg.get("type").and_then(Value::as_str) == Some("response") {
                if let Some(req_seq) = msg.get("request_seq").and_then(Value::as_i64) {
                    let mut guard = out_state.lock().await;
                    guard.injected_seqs.remove(&req_seq)
                } else {
                    false
                }
            } else {
                false
            };

            let (updated, profile_url) = {
                let mut guard = out_state.lock().await;
                let changed = inspect_adapter_event(&msg, &mut guard);
                let url_to_open = if !profile_devtools_opened && guard.mode == "profile" {
                    guard.devtools_uri.clone().or_else(|| {
                        guard
                            .vm_service_uri
                            .as_deref()
                            .and_then(crate::app_runner::build_devtools_performance_url)
                    })
                } else {
                    None
                };
                (changed, url_to_open)
            };

            if updated {
                persist_session(&out_cache, &out_root, ipc_port, &out_state).await;
            }
            if let Some(url) = profile_url {
                profile_devtools_opened = true;
                open_url_in_browser(&url);
            }

            if !suppress && write_dap_message(&mut zed_stdout, &msg).await.is_err() {
                break;
            }
        }
    });

    // Main loop: Read from Zed stdin -> enrich launch/attach -> forward to child adapter
    let mut zed_stdin = BufReader::new(tokio::io::stdin());
    while let Ok(Some(mut msg)) = read_dap_message(&mut zed_stdin).await {
        if let Some((cwd, mode)) = enrich_launch_or_attach(&mut msg, &cache, &project_root) {
            {
                let mut guard = state.lock().await;
                guard.project_root = Some(cwd);
                guard.mode = mode;
            }
            persist_session(&cache, &project_root, ipc_port, &state).await;
        }

        if to_adapter_tx.send(msg).await.is_err() {
            break;
        }
    }

    drop(to_adapter_tx);
    let _ = child.kill().await;
    ipc_handle.abort();
    let _ = adapter_writer_handle.await;
    let _ = adapter_reader_handle.await;
    drop(session_guard);

    Ok(())
}

fn spawn_debug_adapter(
    executable: &str,
    cwd: &Path,
) -> std::io::Result<tokio::process::Child> {
    #[cfg(windows)]
    {
        let lower = executable.to_ascii_lowercase();
        if lower.ends_with(".bat") || lower.ends_with(".cmd") {
            return Command::new("cmd")
                .args(["/C", executable, "debug_adapter"])
                .current_dir(cwd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn();
        }
    }

    Command::new(executable)
        .arg("debug_adapter")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
}

async fn handle_ipc_connection(
    mut stream: tokio::net::TcpStream,
    state: Arc<Mutex<ProxySessionState>>,
    to_adapter_tx: mpsc::Sender<Value>,
) {
    let (read_half, mut write_half) = stream.split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }

    let response = match serde_json::from_str::<IpcRequest>(line.trim()) {
        Ok(IpcRequest::HotReload) => {
            let mut guard = state.lock().await;
            if guard.mode != "debug" {
                IpcResponse {
                    ok: false,
                    message: format!("Hot Reload is disabled in `{}` mode", guard.mode),
                    mode: Some(guard.mode.clone()),
                    vm_service_uri: guard.vm_service_uri.clone(),
                    devtools_uri: guard.devtools_uri.clone(),
                }
            } else {
                let seq = guard.allocate_injected_seq();
                let req = json!({
                    "seq": seq,
                    "type": "request",
                    "command": "hotReload",
                    "arguments": { "reason": "save" }
                });
                let sent = to_adapter_tx.send(req).await.is_ok();
                IpcResponse {
                    ok: sent,
                    message: if sent {
                        "Hot Reload triggered".to_string()
                    } else {
                        "Debug adapter session is closed".to_string()
                    },
                    mode: Some(guard.mode.clone()),
                    vm_service_uri: guard.vm_service_uri.clone(),
                    devtools_uri: guard.devtools_uri.clone(),
                }
            }
        }
        Ok(IpcRequest::HotRestart) => {
            let mut guard = state.lock().await;
            if guard.mode != "debug" {
                IpcResponse {
                    ok: false,
                    message: format!("Hot Restart is disabled in `{}` mode", guard.mode),
                    mode: Some(guard.mode.clone()),
                    vm_service_uri: guard.vm_service_uri.clone(),
                    devtools_uri: guard.devtools_uri.clone(),
                }
            } else {
                let seq = guard.allocate_injected_seq();
                let req = json!({
                    "seq": seq,
                    "type": "request",
                    "command": "hotRestart",
                    "arguments": { "reason": "manual" }
                });
                let sent = to_adapter_tx.send(req).await.is_ok();
                IpcResponse {
                    ok: sent,
                    message: if sent {
                        "Hot Restart triggered".to_string()
                    } else {
                        "Debug adapter session is closed".to_string()
                    },
                    mode: Some(guard.mode.clone()),
                    vm_service_uri: guard.vm_service_uri.clone(),
                    devtools_uri: guard.devtools_uri.clone(),
                }
            }
        }
        Ok(IpcRequest::Status) => {
            let guard = state.lock().await;
            IpcResponse {
                ok: true,
                message: "active".to_string(),
                mode: Some(guard.mode.clone()),
                vm_service_uri: guard.vm_service_uri.clone(),
                devtools_uri: guard.devtools_uri.clone(),
            }
        }
        Err(err) => IpcResponse {
            ok: false,
            message: format!("Invalid IPC request: {err}"),
            mode: None,
            vm_service_uri: None,
            devtools_uri: None,
        },
    };

    if let Ok(mut payload) = serde_json::to_string(&response) {
        payload.push('\n');
        let _ = write_half.write_all(payload.as_bytes()).await;
    }
}

async fn persist_session(
    cache: &DiskCache,
    project_root: &Path,
    port: u16,
    state: &Arc<Mutex<ProxySessionState>>,
) {
    let guard = state.lock().await;
    let record = ActiveSessionRecord {
        project_root: normalize_project_key(project_root),
        port,
        mode: guard.mode.clone(),
        vm_service_uri: guard.vm_service_uri.clone(),
        devtools_uri: guard.devtools_uri.clone(),
        pid: std::process::id(),
    };
    let _ = write_session_record(cache, project_root, &record);
}

struct SessionCleanupGuard {
    cache: DiskCache,
    project_root: PathBuf,
}

impl Drop for SessionCleanupGuard {
    fn drop(&mut self) {
        remove_session_record(&self.cache, &self.project_root);
    }
}

fn open_url_in_browser(url: &str) {
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
