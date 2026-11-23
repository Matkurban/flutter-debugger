use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, Notify, RwLock, mpsc, oneshot};

use crate::cache::{DiskCache, FlutterDevice};
use crate::process_util::{build_command, terminate_child_tree};
use crate::project::{ProjectKind, resolve_sdk_executable};
use crate::runner::strip_ansi_codes;

pub type DevicesChangedCallback = Arc<dyn Fn(Vec<FlutterDevice>) + Send + Sync>;

/// Persistent `flutter daemon` device manager that listens for real-time `device.added` /
/// `device.removed` events with exponential backoff restart and instant `Notify`-based shutdown.
#[derive(Clone)]
pub struct FlutterDaemonManager {
    started: Arc<AtomicBool>,
    shutdown_flag: Arc<AtomicBool>,
    shutdown_notify: Arc<Notify>,
    devices: Arc<RwLock<Vec<FlutterDevice>>>,
    cache: DiskCache,
    next_id: Arc<AtomicI64>,
    stop_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl FlutterDaemonManager {
    pub fn new(cache: DiskCache, devices: Arc<RwLock<Vec<FlutterDevice>>>) -> Self {
        Self {
            started: Arc::new(AtomicBool::new(false)),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            shutdown_notify: Arc::new(Notify::new()),
            devices,
            cache,
            next_id: Arc::new(AtomicI64::new(1)),
            stop_tx: Arc::new(Mutex::new(None)),
        }
    }

    pub fn is_started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }

    /// Lazily starts `flutter daemon` once a Flutter project is opened.
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

    /// Stops the background `flutter daemon` process immediately (including interrupting backoff sleep).
    pub async fn shutdown(&self) {
        self.shutdown_flag.store(true, Ordering::SeqCst);
        self.shutdown_notify.notify_waiters();
        if let Some(tx) = self.stop_tx.lock().await.take() {
            let _ = tx.send(());
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

            let Ok(mut child) = build_command(&executable, ["daemon"], &project_root)
                .kill_on_drop(true)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            else {
                tracing::warn!("Failed to spawn `flutter daemon`; retrying in {backoff_secs}s");
                tokio::select! {
                    _ = self.shutdown_notify.notified() => break,
                    _ = tokio::time::sleep(Duration::from_secs(backoff_secs)) => {}
                }
                backoff_secs = (backoff_secs * 2).min(16);
                continue;
            };

            let (Some(child_stdin), Some(child_stdout)) = (child.stdin.take(), child.stdout.take())
            else {
                terminate_child_tree(&mut child).await;
                break;
            };

            let (tx, mut rx) = mpsc::channel::<String>(32);
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

            let enable_id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let enable_cmd = format!(
                "{}\n",
                json!([{ "id": enable_id, "method": "device.enable" }])
            );
            let _ = tx.send(enable_cmd).await;

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
                        terminate_child_tree(&mut child).await;
                        break;
                    }
                    _ = self.shutdown_notify.notified() => {
                        terminate_child_tree(&mut child).await;
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
                                    if apply_daemon_device_event(&msg, &mut live_map) {
                                        changed = true;
                                    }
                                }
                                if changed {
                                    let mut sorted: Vec<FlutterDevice> =
                                        live_map.values().cloned().collect();
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
            terminate_child_tree(&mut child).await;

            if self.shutdown_flag.load(Ordering::SeqCst) {
                break;
            }

            tokio::select! {
                _ = self.shutdown_notify.notified() => break,
                _ = tokio::time::sleep(Duration::from_secs(backoff_secs)) => {}
            }
            backoff_secs = (backoff_secs * 2).min(16);
        }
    }
}

pub fn parse_daemon_messages(line: &str) -> Option<Vec<Value>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return None;
    }
    serde_json::from_str::<Vec<Value>>(trimmed).ok()
}

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

            devices.insert(
                id.to_string(),
                FlutterDevice {
                    id: id.to_string(),
                    name,
                    target_platform,
                    emulator,
                    sdk,
                },
            );
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
}
