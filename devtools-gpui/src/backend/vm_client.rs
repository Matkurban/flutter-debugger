use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::backend::models::{
    FrameTiming, HttpRequestDetail, HttpRequestRecord, LogEntry, LogKind, MemorySnapshot,
    WidgetProperty, WidgetTreeNode, parse_widget_properties,
};

#[derive(Debug, Clone)]
pub enum VmPushEvent {
    Frame(FrameTiming),
    Log(LogEntry),
    ServiceExtensionStateChanged {
        extension: String,
        value: String,
    },
    ServiceExtensionAdded {
        rpc: String,
    },
    IsolateReloaded,
    GcNotification,
    Disconnected(String),
}

#[derive(Debug, Clone)]
pub struct VmMetadata {
    pub main_isolate_name: String,
    pub target_cpu: String,
    pub arch_bits: u64,
}

type PendingRpcMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// Full-duplex Dart VM Service JSON-RPC 2.0 WebSocket client built on `tokio-tungstenite`.
#[derive(Clone)]
pub struct DartVmClient {
    ws_uri: String,
    write_tx: mpsc::Sender<String>,
    pending: PendingRpcMap,
    next_id: Arc<AtomicU64>,
    main_isolate_id: Arc<tokio::sync::RwLock<String>>,
}

impl DartVmClient {
    pub async fn connect(
        raw_uri: &str,
        push_tx: mpsc::UnboundedSender<VmPushEvent>,
        log_seq: Arc<AtomicU64>,
    ) -> Result<(Self, VmMetadata), String> {
        let ws_uri = normalize_to_ws_uri(raw_uri)?;

        let (ws_stream, _) =
            tokio::time::timeout(Duration::from_secs(4), tokio_tungstenite::connect_async(&ws_uri))
                .await
                .map_err(|_| format!("Timed out connecting to Dart VM Service at {ws_uri}"))?
                .map_err(|e| format!("Failed to connect to Dart VM Service at {ws_uri}: {e}"))?;

        let (mut sink, mut stream) = ws_stream.split();
        let (write_tx, mut write_rx) = mpsc::channel::<String>(256);
        let pending: PendingRpcMap = Arc::new(Mutex::new(HashMap::new()));

        // Independent writer task
        tokio::spawn(async move {
            while let Some(msg) = write_rx.recv().await {
                if sink.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }
        });

        // Independent reader task
        let pending_reader = Arc::clone(&pending);
        let push_reader = push_tx.clone();
        let seq_reader = Arc::clone(&log_seq);
        tokio::spawn(async move {
            while let Some(frame_res) = stream.next().await {
                let text = match frame_res {
                    Ok(Message::Text(t)) => t.to_string(),
                    Ok(Message::Binary(b)) => String::from_utf8_lossy(&b).into_owned(),
                    Ok(Message::Close(_)) => {
                        let _ = push_reader
                            .send(VmPushEvent::Disconnected("VM Service closed connection".into()));
                        break;
                    }
                    Ok(_) => continue,
                    Err(e) => {
                        let _ = push_reader
                            .send(VmPushEvent::Disconnected(format!("WebSocket error: {e}")));
                        break;
                    }
                };

                let Ok(val) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };

                // Route RPC response by `id`
                if let Some(id) = parse_rpc_id(val.get("id")) {
                    if let Some(tx) = pending_reader.lock().await.remove(&id) {
                        if let Some(err) = val.get("error") {
                            let msg = err
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("VM Service RPC error");
                            let details = err
                                .get("data")
                                .and_then(|d| d.get("details"))
                                .and_then(Value::as_str);
                            let full_err = match details {
                                Some(d) => format!("{msg}: {d}"),
                                None => msg.to_string(),
                            };
                            let _ = tx.send(Err(full_err));
                        } else {
                            let result = val.get("result").cloned().unwrap_or(Value::Null);
                            let _ = tx.send(Ok(result));
                        }
                    }
                    continue;
                }

                // Route `streamNotify` events
                if val.get("method").and_then(Value::as_str) == Some("streamNotify")
                    && let Some(params) = val.get("params")
                {
                    handle_stream_notify(params, &push_reader, &seq_reader);
                }
            }

            // Fail any remaining pending requests on disconnect
            let mut map = pending_reader.lock().await;
            for (_, tx) in map.drain() {
                let _ = tx.send(Err("VM Service disconnected".to_string()));
            }
        });

        let client = Self {
            ws_uri,
            write_tx,
            pending,
            next_id: Arc::new(AtomicU64::new(1)),
            main_isolate_id: Arc::new(tokio::sync::RwLock::new(String::new())),
        };

        let metadata = client.initialize_session().await?;
        Ok((client, metadata))
    }

    pub fn ws_uri(&self) -> &str {
        &self.ws_uri
    }

    pub async fn main_isolate_id(&self) -> String {
        self.main_isolate_id.read().await.clone()
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let payload = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id.to_string(),
            "method": method,
            "params": params,
        }))
        .map_err(|e| e.to_string())?;

        if self.write_tx.send(payload).await.is_err() {
            self.pending.lock().await.remove(&id);
            return Err("VM Service write channel closed".to_string());
        }

        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .map_err(|_| format!("VM Service RPC `{method}` timed out"))?
            .map_err(|_| format!("VM Service RPC `{method}` cancelled"))?
    }

    pub async fn call_isolate(&self, method: &str, mut params: Value) -> Result<Value, String> {
        let isolate_id = self.main_isolate_id().await;
        if isolate_id.is_empty() {
            return Err("No active main isolate ID".to_string());
        }
        if let Some(obj) = params.as_object_mut() {
            obj.insert("isolateId".to_string(), json!(isolate_id));
        } else {
            params = json!({ "isolateId": isolate_id });
        }
        self.call(method, params).await
    }

    async fn initialize_session(&self) -> Result<VmMetadata, String> {
        let vm = self.call("getVM", json!({})).await?;
        let target_cpu = vm
            .get("targetCPU")
            .and_then(Value::as_str)
            .or_else(|| vm.get("hostCPU").and_then(Value::as_str))
            .unwrap_or("unknown")
            .to_string();
        let arch_bits = vm
            .get("architectureBits")
            .and_then(Value::as_u64)
            .unwrap_or(64);

        let isolates = vm
            .get("isolates")
            .and_then(Value::as_array)
            .ok_or_else(|| "VM Service `getVM` returned no isolates".to_string())?;

        let chosen = isolates
            .iter()
            .find(|iso| {
                iso.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| n.contains("main"))
            })
            .or_else(|| isolates.first())
            .ok_or_else(|| "No running Dart isolate found in VM".to_string())?;

        let main_isolate_id = chosen
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "Isolate entry missing `id`".to_string())?
            .to_string();
        let main_isolate_name = chosen
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("main")
            .to_string();

        *self.main_isolate_id.write().await = main_isolate_id.clone();

        // Warm isolate details (including registered extensionRPCs)
        let _ = self
            .call("getIsolate", json!({ "isolateId": main_isolate_id }))
            .await;

        // Subscribe to all required streams in parallel
        let streams = ["Logging", "Stdout", "Stderr", "Extension", "Isolate", "GC"];
        let mut listen_futs = Vec::with_capacity(streams.len());
        for stream_id in streams {
            let client = self.clone();
            listen_futs.push(tokio::spawn(async move {
                let _ = client
                    .call("streamListen", json!({ "streamId": stream_id }))
                    .await;
            }));
        }
        for f in listen_futs {
            let _ = f.await;
        }

        // Enable HTTP timeline / profile logging
        self.enable_http_profiling(&main_isolate_id).await;

        Ok(VmMetadata {
            main_isolate_name,
            target_cpu,
            arch_bits,
        })
    }

    pub async fn refresh_main_isolate(&self) -> Result<VmMetadata, String> {
        let vm = self.call("getVM", json!({})).await?;
        let target_cpu = vm
            .get("targetCPU")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let arch_bits = vm
            .get("architectureBits")
            .and_then(Value::as_u64)
            .unwrap_or(64);

        if let Some(isolates) = vm.get("isolates").and_then(Value::as_array)
            && let Some(chosen) = isolates
                .iter()
                .find(|iso| {
                    iso.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|n| n.contains("main"))
                })
                .or_else(|| isolates.first())
            && let Some(id) = chosen.get("id").and_then(Value::as_str)
        {
            let name = chosen
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("main")
                .to_string();
            *self.main_isolate_id.write().await = id.to_string();
            let _ = self
                .call("getIsolate", json!({ "isolateId": id }))
                .await;
            self.enable_http_profiling(id).await;
            return Ok(VmMetadata {
                main_isolate_name: name,
                target_cpu,
                arch_bits,
            });
        }
        Err("No main isolate found".to_string())
    }

    async fn enable_http_profiling(&self, isolate_id: &str) {
        let res = self
            .call(
                "httpEnableTimelineLogging",
                json!({ "isolateId": isolate_id, "enabled": true }),
            )
            .await;
        if res.is_err() {
            let _ = self
                .call(
                    "ext.dart.io.httpEnableTimelineLogging",
                    json!({ "isolateId": isolate_id, "enabled": true }),
                )
                .await;
        }
    }

    /// Incremental poll of `getHttpProfile` (with fallback to `ext.dart.io.getHttpProfile`).
    pub async fn poll_http_profile(
        &self,
        updated_since: Option<u64>,
    ) -> Result<(u64, Vec<HttpRequestRecord>), String> {
        let mut params = json!({});
        if let Some(ts) = updated_since {
            params["updatedSince"] = json!(ts);
        }

        let resp = match self.call_isolate("getHttpProfile", params.clone()).await {
            Ok(v) => v,
            Err(_) => self.call_isolate("ext.dart.io.getHttpProfile", params).await?,
        };

        let timestamp = resp.get("timestamp").and_then(Value::as_u64).unwrap_or(0);
        let requests = resp
            .get("requests")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(HttpRequestRecord::parse_from_profile_entry)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Ok((timestamp, requests))
    }

    /// Fetches full headers and request/response body for a single HTTP request ID.
    pub async fn get_http_profile_request(&self, id: &str) -> Result<HttpRequestDetail, String> {
        let id_param: Value = match id.parse::<u64>() {
            Ok(num) => json!(num),
            Err(_) => json!(id),
        };
        let params = json!({ "id": id });
        let resp = match self
            .call_isolate("getHttpProfileRequest", params.clone())
            .await
        {
            Ok(v) => v,
            Err(_) => {
                match self
                    .call_isolate("getHttpProfileRequest", json!({ "id": id_param }))
                    .await
                {
                    Ok(v) => v,
                    Err(_) => {
                        self.call_isolate("ext.dart.io.getHttpProfileRequest", params)
                            .await?
                    }
                }
            }
        };
        Ok(HttpRequestRecord::parse_detail_from_response(&resp))
    }

    pub async fn clear_http_profile(&self) -> Result<(), String> {
        if self
            .call_isolate("clearHttpProfile", json!({}))
            .await
            .is_err()
        {
            let _ = self
                .call_isolate("ext.dart.io.clearHttpProfile", json!({}))
                .await;
        }
        Ok(())
    }

    /// Fetches the Flutter Widget Summary Tree via `ext.flutter.inspector.getRootWidgetSummaryTree`.
    pub async fn fetch_widget_summary_tree(&self, group_name: &str) -> Result<WidgetTreeNode, String> {
        let resp = match self
            .call_isolate(
                "ext.flutter.inspector.getRootWidgetSummaryTree",
                json!({ "objectGroup": group_name }),
            )
            .await
        {
            Ok(v) => v,
            Err(_) => {
                self.call_isolate(
                    "ext.flutter.inspector.getRootWidget",
                    json!({ "objectGroup": group_name }),
                )
                .await?
            }
        };

        WidgetTreeNode::parse_from_json(&resp)
            .ok_or_else(|| "Inspector returned empty widget tree".to_string())
    }

    /// Fetches properties for a selected widget node (`valueId`).
    pub async fn fetch_widget_properties(
        &self,
        value_id: &str,
        group_name: &str,
    ) -> Result<Vec<WidgetProperty>, String> {
        if value_id.is_empty() {
            return Ok(Vec::new());
        }
        let resp = self
            .call_isolate(
                "ext.flutter.inspector.getProperties",
                json!({
                    "arg": value_id,
                    "objectGroup": group_name,
                }),
            )
            .await?;

        Ok(parse_widget_properties(&resp))
    }

    /// Polls `getMemoryUsage` + `getVM` RSS for the active isolate.
    pub async fn poll_memory_usage(&self, is_gc_event: bool) -> Result<MemorySnapshot, String> {
        let rss = self
            .call("getVM", json!({}))
            .await
            .ok()
            .and_then(|v| v.get("currentRSS").and_then(Value::as_u64))
            .unwrap_or(0);
        let mem = self.call_isolate("getMemoryUsage", json!({})).await?;
        MemorySnapshot::parse_memory_usage(&mem, rss, is_gc_event)
            .ok_or_else(|| "Failed to parse getMemoryUsage response".to_string())
    }

    /// Forces garbage collection via `getAllocationProfile(isolateId, gc: true)`.
    pub async fn trigger_force_gc(&self) -> Result<MemorySnapshot, String> {
        let _ = self
            .call_isolate("getAllocationProfile", json!({ "gc": "true" }))
            .await;
        self.poll_memory_usage(true).await
    }
}

fn parse_rpc_id(val: Option<&Value>) -> Option<u64> {
    match val? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse::<u64>().ok(),
        _ => None,
    }
}

fn handle_stream_notify(
    params: &Value,
    push_tx: &mpsc::UnboundedSender<VmPushEvent>,
    log_seq: &AtomicU64,
) {
    let stream_id = params
        .get("streamId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(event) = params.get("event") else {
        return;
    };
    let kind = event
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();

    match stream_id {
        "Stdout" | "Stderr" => {
            if kind == "WriteEvent"
                && let Some(b64) = event.get("bytes").and_then(Value::as_str)
                && let Some(decoded) = decode_base64_bytes(b64)
            {
                let text = String::from_utf8_lossy(&decoded).into_owned();
                for line in text.lines() {
                    let trimmed = line.trim_end();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let seq = log_seq.fetch_add(1, Ordering::Relaxed);
                    let log_kind = if stream_id == "Stderr" {
                        LogKind::Stderr
                    } else {
                        LogKind::Stdout
                    };
                    let entry =
                        LogEntry::from_text(seq, log_kind, stream_id, trimmed.to_string(), None);
                    let _ = push_tx.send(VmPushEvent::Log(entry));
                }
            }
        }
        "Logging" => {
            let seq = log_seq.fetch_add(1, Ordering::Relaxed);
            if let Some(entry) = LogEntry::parse_logging_stream_event(event, seq) {
                let _ = push_tx.send(VmPushEvent::Log(entry));
            }
        }
        "GC" => {
            let reason = event
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("collect");
            let seq = log_seq.fetch_add(1, Ordering::Relaxed);
            let entry = LogEntry::from_text(
                seq,
                LogKind::Gc,
                "GC",
                format!("Dart VM Garbage Collection ({reason})"),
                None,
            );
            let _ = push_tx.send(VmPushEvent::Log(entry));
            let _ = push_tx.send(VmPushEvent::GcNotification);
        }
        "Extension" => {
            let ext_kind = event
                .get("extensionKind")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let ext_data = event.get("extensionData").unwrap_or(&Value::Null);

            match ext_kind {
                "Flutter.Frame" => {
                    if let Some(frame) = FrameTiming::parse_flutter_frame(ext_data) {
                        let _ = push_tx.send(VmPushEvent::Frame(frame));
                    }
                }
                "Flutter.Error" => {
                    let seq = log_seq.fetch_add(1, Ordering::Relaxed);
                    let entry = LogEntry::parse_flutter_error_extension(ext_data, seq);
                    let _ = push_tx.send(VmPushEvent::Log(entry));
                }
                "Flutter.ServiceExtensionStateChanged" => {
                    if let Some(extension) = ext_data.get("extension").and_then(Value::as_str) {
                        let val_str = match ext_data.get("value") {
                            Some(Value::String(s)) => s.clone(),
                            Some(other) => other.to_string(),
                            None => String::new(),
                        };
                        let _ = push_tx.send(VmPushEvent::ServiceExtensionStateChanged {
                            extension: extension.to_string(),
                            value: val_str,
                        });
                    }
                }
                "Flutter.FirstFrame" | "Flutter.FrameworkInitialization" => {
                    let seq = log_seq.fetch_add(1, Ordering::Relaxed);
                    let entry = LogEntry::from_text(
                        seq,
                        LogKind::FlutterInfo,
                        "Flutter",
                        format!("Event: {ext_kind}"),
                        None,
                    );
                    let _ = push_tx.send(VmPushEvent::Log(entry));
                }
                _ => {}
            }
        }
        "Isolate" => match kind {
            "ServiceExtensionAdded" => {
                if let Some(rpc) = event.get("extensionRPC").and_then(Value::as_str) {
                    let _ = push_tx.send(VmPushEvent::ServiceExtensionAdded {
                        rpc: rpc.to_string(),
                    });
                }
            }
            "IsolateReload" | "IsolateRunnable" => {
                let _ = push_tx.send(VmPushEvent::IsolateReloaded);
            }
            _ => {}
        },
        _ => {}
    }
}

fn decode_base64_bytes(input: &str) -> Option<Vec<u8>> {
    let trimmed = input.trim();
    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for b in trimmed.bytes() {
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        buf = (buf << 6) | (val as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

/// Normalizes a Dart VM Service HTTP or WebSocket URI into a `ws://.../ws` (or `wss://.../ws`) endpoint.
pub fn normalize_to_ws_uri(uri: &str) -> Result<String, String> {
    let trimmed = uri.trim();
    if trimmed.is_empty() {
        return Err("Empty VM Service URI".to_string());
    }

    let with_scheme = if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        trimmed.to_string()
    } else {
        return Err(format!("Unsupported VM Service URI scheme: `{trimmed}`"));
    };

    let mut parsed =
        url::Url::parse(&with_scheme).map_err(|e| format!("Invalid VM Service URI: {e}"))?;
    let path = parsed.path().trim_end_matches('/').to_string();
    if !path.ends_with("/ws") {
        parsed.set_path(&format!("{path}/ws"));
    }
    Ok(parsed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_http_and_ws_vm_service_uris() {
        assert_eq!(
            normalize_to_ws_uri("http://127.0.0.1:54321/abcdef=/").unwrap(),
            "ws://127.0.0.1:54321/abcdef=/ws"
        );
        assert_eq!(
            normalize_to_ws_uri("http://127.0.0.1:54321/abcdef=/ws").unwrap(),
            "ws://127.0.0.1:54321/abcdef=/ws"
        );
        assert_eq!(
            normalize_to_ws_uri("ws://localhost:8181").unwrap(),
            "ws://localhost:8181/ws"
        );
        assert_eq!(
            normalize_to_ws_uri("https://127.0.0.1:9100/token").unwrap(),
            "wss://127.0.0.1:9100/token/ws"
        );
        assert!(normalize_to_ws_uri("").is_err());
    }
}
