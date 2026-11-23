use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};

use crate::cache::{DiskCache, FlutterDevice};
use crate::emulators::FlutterEmulator;
use crate::project::{ProjectKind, resolve_sdk_executable};
use crate::runner::strip_ansi_codes;

pub type DevicesChangedCallback = Arc<dyn Fn(Vec<FlutterDevice>) + Send + Sync>;

/// Persistent `flutter daemon` device manager that listens for real-time `device.added` /
/// `device.removed` events with exponential backoff restart and fallback support.
#[derive(Clone)]
pub struct FlutterDaemonManager {
    started: Arc<AtomicBool>,
    shutdown_flag: Arc<AtomicBool>,
    devices: Arc<RwLock<Vec<FlutterDevice>>>,
    cache: DiskCache,
    rpc_tx: Arc<RwLock<Option<mpsc::Sender<String>>>>,
    pending_rpc: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>,
    next_id: Arc<AtomicI64>,
    stop_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl FlutterDaemonManager {
    pub fn new(cache: DiskCache, devices: Arc<RwLock<Vec<FlutterDevice>>>) -> Self {
        Self {
            started: Arc::new(AtomicBool::new(false)),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            devices,
            cache,
            rpc_tx: Arc::new(RwLock::new(None)),
            pending_rpc: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicI64::new(1)),
            stop_tx: Arc::new(Mutex::new(None)),
        }
    }

    /// Lazily starts `flutter daemon` once a Flutter project is opened.
    /// Does nothing if already started.
    pub fn ensure_started(
        &self,
        project_root: PathBuf,
        custom_sdk_path: Option<String>,
        on_devices_changed: Option<DevicesChangedCallback>,
    ) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }

        let this = self.clone();
        tokio::spawn(async move {
            this.supervise_loop(project_root, custom_sdk_path, on_devices_changed)
                .await;
        });
    }

    /// Queries available emulators via the running daemon, if connected.
    #[allow(dead_code)]
    pub async fn get_emulators(&self) -> Option<Vec<FlutterEmulator>> {
        let resp = self.send_rpc("emulator.getEmulators", json!({})).await?;
        parse_daemon_emulators_result(&resp)
    }

    /// Stops the background `flutter daemon` process cleanly.
    pub async fn shutdown(&self) {
        self.shutdown_flag.store(true, Ordering::SeqCst);
        if let Some(tx) = self.stop_tx.lock().await.take() {
            let _ = tx.send(());
        }
        *self.rpc_tx.write().await = None;
    }

    async fn send_rpc(&self, method: &str, params: Value) -> Option<Value> {
        let tx = self.rpc_tx.read().await.clone()?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (resp_tx, resp_rx) = oneshot::channel();
        self.pending_rpc.lock().await.insert(id, resp_tx);

        let payload = json!([{
            "id": id,
            "method": method,
            "params": params
        }]);
        let mut line = serde_json::to_string(&payload).ok()?;
        line.push('\n');

        if tx.send(line).await.is_err() {
            self.pending_rpc.lock().await.remove(&id);
            return None;
        }

        match tokio::time::timeout(Duration::from_secs(5), resp_rx).await {
            Ok(Ok(val)) => Some(val),
            _ => {
                self.pending_rpc.lock().await.remove(&id);
                None
            }
        }
    }

    async fn supervise_loop(
        &self,
        project_root: PathBuf,
        custom_sdk_path: Option<String>,
        on_devices_changed: Option<DevicesChangedCallback>,
    ) {
        let mut backoff_secs = 1u64;

        while !self.shutdown_flag.load(Ordering::SeqCst) {
            let executable = resolve_sdk_executable(
                &project_root,
                ProjectKind::Flutter,
                custom_sdk_path.as_deref(),
            );

            let Ok(mut child) = spawn_flutter_daemon(&executable, &project_root) else {
                tracing::warn!("Failed to spawn `flutter daemon`; retrying in {backoff_secs}s");
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(16);
                continue;
            };

            let (Some(child_stdin), Some(child_stdout)) = (child.stdin.take(), child.stdout.take())
            else {
                let _ = child.kill().await;
                break;
            };

            let (tx, mut rx) = mpsc::channel::<String>(32);
            *self.rpc_tx.write().await = Some(tx.clone());

            let (stop_tx, stop_rx) = oneshot::channel::<()>();
            *self.stop_tx.lock().await = Some(stop_tx);

            let stdin_task = tokio::spawn(async move {
                let mut writer = child_stdin;
                while let Some(line) = rx.recv().await {
                    if writer.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                    let _ = writer.flush().await;
                }
            });

            // Enable real-time device polling events
            let enable_id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let enable_cmd = format!(
                "{}\n",
                json!([{ "id": enable_id, "method": "device.enable" }])
            );
            let _ = tx.send(enable_cmd).await;

            // Reset backoff once daemon started cleanly
            backoff_secs = 1;

            let mut reader = BufReader::new(child_stdout);
            let mut line = String::new();
            let mut live_map: HashMap<String, FlutterDevice> = HashMap::new();
            tokio::pin!(stop_rx);

            loop {
                line.clear();
                tokio::select! {
                    biased;
                    _ = &mut stop_rx => {
                        terminate_daemon_child(&mut child).await;
                        break;
                    }
                    res = reader.read_line(&mut line) => {
                        match res {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {
                                let cleaned = strip_ansi_codes(&line);
                                let Some(events) = parse_daemon_messages(&cleaned) else {
                                    continue;
                                };
                                let mut changed = false;
                                for msg in events {
                                    if let Some(id) = msg.get("id").and_then(Value::as_i64)
                                        && let Some(sender) = self.pending_rpc.lock().await.remove(&id)
                                    {
                                        let _ = sender.send(msg);
                                        continue;
                                    }
                                    if apply_daemon_device_event(&msg, &mut live_map) {
                                        changed = true;
                                    }
                                }
                                if changed {
                                    let mut sorted: Vec<FlutterDevice> = live_map.values().cloned().collect();
                                    sorted.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
                                    self.cache.save_devices(&sorted);
                                    *self.devices.write().await = sorted.clone();
                                    if let Some(cb) = &on_devices_changed {
                                        cb(sorted);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            stdin_task.abort();
            *self.rpc_tx.write().await = None;
            let _ = child.kill().await;
            let _ = child.wait().await;

            if self.shutdown_flag.load(Ordering::SeqCst) {
                break;
            }

            tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(16);
        }
    }
}

/// Parses a `flutter daemon` stdout line (JSON array of event/response objects).
pub fn parse_daemon_messages(line: &str) -> Option<Vec<Value>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return None;
    }
    serde_json::from_str::<Vec<Value>>(trimmed).ok()
}

/// Applies a `device.added` or `device.removed` event to `devices`. Returns `true` if modified.
pub fn apply_daemon_device_event(
    msg: &Value,
    devices: &mut HashMap<String, FlutterDevice>,
) -> bool {
    let Some(event) = msg.get("event").and_then(Value::as_str) else {
        return false;
    };
    let Some(params) = msg.get("params") else {
        return false;
    };

    match event {
        "device.added" => {
            let Some(id) = params.get("id").and_then(Value::as_str) else {
                return false;
            };
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(id)
                .to_string();
            let target_platform = params
                .get("platform")
                .or_else(|| params.get("targetPlatform"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let emulator = params
                .get("emulator")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let sdk = params
                .get("sdk")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();

            let dev = FlutterDevice {
                id: id.to_string(),
                name,
                target_platform,
                emulator,
                sdk,
            };
            devices.insert(id.to_string(), dev);
            true
        }
        "device.removed" => {
            if let Some(id) = params.get("id").and_then(Value::as_str) {
                devices.remove(id).is_some()
            } else {
                false
            }
        }
        _ => false,
    }
}

pub fn parse_daemon_emulators_result(msg: &Value) -> Option<Vec<FlutterEmulator>> {
    let arr = msg.get("result")?.as_array()?;
    let mut out = Vec::new();
    for item in arr {
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_string();
        let platform = item
            .get("platformType")
            .or_else(|| item.get("category"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        out.push(FlutterEmulator {
            id: id.to_string(),
            name,
            platform,
        });
    }
    Some(out)
}

fn spawn_flutter_daemon(executable: &str, cwd: &Path) -> std::io::Result<Child> {
    #[cfg(windows)]
    {
        let lower = executable.to_ascii_lowercase();
        if lower.ends_with(".bat") || lower.ends_with(".cmd") {
            return Command::new("cmd")
                .args(["/C", executable, "daemon"])
                .current_dir(cwd)
                .kill_on_drop(true)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();
        }
    }

    Command::new(executable)
        .arg("daemon")
        .current_dir(cwd)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
}

async fn terminate_daemon_child(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let _ = Command::new("pkill")
            .args(["-TERM", "-P", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }

    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_device_added_and_removed_events() {
        let mut map = HashMap::new();
        let added = json!({
            "event": "device.added",
            "params": {
                "id": "macos",
                "name": "macOS",
                "platform": "darwin",
                "emulator": false
            }
        });
        assert!(apply_daemon_device_event(&added, &mut map));
        assert_eq!(map.len(), 1);
        assert_eq!(map["macos"].target_platform, "darwin");

        let removed = json!({
            "event": "device.removed",
            "params": { "id": "macos" }
        });
        assert!(apply_daemon_device_event(&removed, &mut map));
        assert!(map.is_empty());
    }

    #[test]
    fn parses_daemon_emulators_response() {
        let resp = json!({
            "id": 2,
            "result": [
                { "id": "apple_ios_simulator", "name": "iOS Simulator", "platformType": "ios" },
                { "id": "Pixel_8", "name": "Pixel 8", "platformType": "android" }
            ]
        });
        let emulators = parse_daemon_emulators_result(&resp).expect("emulators");
        assert_eq!(emulators.len(), 2);
        assert_eq!(emulators[0].id, "apple_ios_simulator");
        assert_eq!(emulators[1].platform, "android");
    }
}
