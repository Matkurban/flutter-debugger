use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, Notify, RwLock, mpsc, oneshot};

use crate::cache::{DiskCache, FlutterDevice};
use crate::process_util::{build_command, terminate_child_tree};
use crate::project::{ProjectKind, resolve_sdk_executable};
use crate::runner::strip_ansi_codes;

pub type DevicesChangedCallback = Arc<dyn Fn(Vec<FlutterDevice>) + Send + Sync>;

/// Grace window during which initial `device.added` events from a newly spawned `flutter daemon`
/// are merged with the existing device cache rather than overwriting a multi-device list with 1 entry.
const INITIAL_DISCOVERY_GRACE: Duration = Duration::from_secs(3);

/// Persistent `flutter daemon` device manager that listens for real-time `device.added` /
/// `device.removed` events with exponential backoff restart and synchronous shutdown wait.
#[derive(Clone)]
pub struct FlutterDaemonManager {
    started: Arc<AtomicBool>,
    shutdown_flag: Arc<AtomicBool>,
    shutdown_notify: Arc<Notify>,
    devices: Arc<RwLock<Vec<FlutterDevice>>>,
    cache: DiskCache,
    next_id: Arc<AtomicI64>,
    stop_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    task_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
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
            task_handle: Arc::new(Mutex::new(None)),
        }
    }

    pub fn is_started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }

    /// Lazily starts `flutter daemon` once a Flutter project is discovered or opened.
    pub fn ensure_started(
        &self,
        project_root: PathBuf,
        custom_sdk_path: Option<String>,
        on_devices_changed: Option<DevicesChangedCallback>,
    ) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        self.shutdown_flag.store(false, Ordering::SeqCst);

        let this = self.clone();
        let task_handle_slot = Arc::clone(&self.task_handle);
        let handle = tokio::spawn(async move {
            this.supervise_loop(project_root, custom_sdk_path, on_devices_changed)
                .await;
        });
        tokio::spawn(async move {
            *task_handle_slot.lock().await = Some(handle);
        });
    }

    /// Stops the background `flutter daemon` process and waits up to 2 seconds for
    /// `terminate_child_tree` to finish so a subsequent server restart never races the old daemon.
    pub async fn shutdown(&self) {
        self.shutdown_flag.store(true, Ordering::SeqCst);
        self.shutdown_notify.notify_waiters();
        if let Some(tx) = self.stop_tx.lock().await.take() {
            let _ = tx.send(());
        }
        if let Some(mut handle) = self.task_handle.lock().await.take()
            && tokio::time::timeout(Duration::from_secs(2), &mut handle)
                .await
                .is_err()
        {
            handle.abort();
        }
        self.started.store(false, Ordering::SeqCst);
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
            let daemon_started_at = Instant::now();
            let mut saw_removal = false;
            let mut grace_flushed = false;
            let grace_sleep = tokio::time::sleep(INITIAL_DISCOVERY_GRACE);
            tokio::pin!(stop_rx);
            tokio::pin!(grace_sleep);

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
                    _ = &mut grace_sleep, if !grace_flushed => {
                        grace_flushed = true;
                        if !live_map.is_empty() {
                            let mut sorted: Vec<FlutterDevice> = live_map.values().cloned().collect();
                            sorted.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
                            self.cache.save_devices(&sorted);
                            *self.devices.write().await = sorted.clone();
                            if let Some(cb) = &on_devices_changed {
                                cb(sorted);
                            }
                        }
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
                                    if msg.get("event").and_then(Value::as_str) == Some("device.removed") {
                                        saw_removal = true;
                                    }
                                    if apply_daemon_device_event(&msg, &mut live_map) {
                                        changed = true;
                                    }
                                }
                                if changed {
                                    let in_grace = !grace_flushed
                                        && daemon_started_at.elapsed() < INITIAL_DISCOVERY_GRACE;
                                    let cached_snapshot = {
                                        let mem = self.devices.read().await.clone();
                                        if !mem.is_empty() {
                                            mem
                                        } else {
                                            self.cache.load_devices().map(|c| c.devices).unwrap_or_default()
                                        }
                                    };
                                    let sorted = merge_daemon_devices_with_grace(
                                        &live_map,
                                        &cached_snapshot,
                                        in_grace,
                                        saw_removal,
                                    );
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

/// Merges live `flutter daemon` devices with existing cached devices during the initial
/// discovery grace window (unless a `device.removed` event was explicitly received),
/// preventing the very first `device.added` event from wiping out a multi-device cache.
pub fn merge_daemon_devices_with_grace(
    live_map: &HashMap<String, FlutterDevice>,
    cached_devices: &[FlutterDevice],
    in_grace_period: bool,
    saw_removal: bool,
) -> Vec<FlutterDevice> {
    let mut merged: HashMap<String, FlutterDevice> = if in_grace_period && !saw_removal {
        cached_devices
            .iter()
            .map(|d| (d.id.clone(), d.clone()))
            .collect()
    } else {
        HashMap::new()
    };

    for (id, dev) in live_map {
        merged.insert(id.clone(), dev.clone());
    }

    let mut sorted: Vec<FlutterDevice> = merged.into_values().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    sorted
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

    #[test]
    fn initial_grace_period_merges_first_device_added_with_cached_devices() {
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

        let mut live_map = HashMap::new();
        let added = json!({
            "event": "device.added",
            "params": {
                "id": "macos",
                "name": "macOS",
                "platform": "darwin",
                "emulator": false
            }
        });
        assert!(apply_daemon_device_event(&added, &mut live_map));

        // During grace period (no removal seen), 1st device.added MUST preserve cached Chrome device
        let during_grace = merge_daemon_devices_with_grace(&live_map, &cached, true, false);
        assert_eq!(during_grace.len(), 2);

        // Once grace period ends or a removal is seen, live_map is authoritative
        let after_grace = merge_daemon_devices_with_grace(&live_map, &cached, false, false);
        assert_eq!(after_grace.len(), 1);
        assert_eq!(after_grace[0].id, "macos");
    }
}
