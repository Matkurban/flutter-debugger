use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use flutter_debugger_protocol::{ExceptionPauseMode, OverlayState, ServiceOverlay};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::backend::error::DevToolsError;
use crate::protocol::{
    FrameTiming, HttpRequestDetail, HttpRequestRecord, LayoutExplorerNode, LogEntry, LogKind,
    MemorySnapshot, ParsedLogRecord, WebSocketConnectionDetail, WidgetProperty, WidgetTreeNode,
    build_pub_root_directories_params, parse_process_memory_rss, parse_widget_children_from_json,
    parse_widget_properties,
};

#[derive(Debug, Clone)]
pub enum VmPushEvent {
    Frame(FrameTiming),
    Log(LogEntry),
    LogWithTruncatedRefs(ParsedLogRecord),
    ServiceExtensionStateChanged { extension: String, value: String },
    ServiceExtensionAdded { rpc: String },
    WidgetInspectEvent,
    IsolateReloaded,
    GcNotification,
    Disconnected(String),
}
#[derive(Debug, Clone)]
pub struct VmMetadata { pub main_isolate_name: String, pub target_cpu: String, pub arch_bits: u64 }
type PendingRpcMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, DevToolsError>>>>>;
#[derive(Clone)]
pub struct DartVmClient {
    ws_uri: String,
    write_tx: mpsc::Sender<String>,
    pending: PendingRpcMap,
    next_id: Arc<AtomicU64>,
    main_isolate_id: Arc<RwLock<String>>,
    isolate_ids: Arc<RwLock<Vec<String>>>,
    extension_rpcs: Arc<RwLock<HashSet<String>>>,
    process_memory_supported: Arc<AtomicBool>,
}

impl DartVmClient {
    pub async fn connect(
        raw_uri: &str,
        push_tx: mpsc::UnboundedSender<VmPushEvent>,
        log_seq: Arc<AtomicU64>,
    ) -> Result<(Self, VmMetadata), DevToolsError> {
        let ws_uri = normalize_to_ws_uri(raw_uri)?;
        let (ws_stream, _) = tokio::time::timeout(Duration::from_secs(4), tokio_tungstenite::connect_async(&ws_uri))
            .await
            .map_err(|_| DevToolsError::Connection(format!("Timed out connecting to {ws_uri}")))?
            .map_err(|e| DevToolsError::Connection(format!("Failed to connect to {ws_uri}: {e}")))?;

        let (mut sink, mut stream) = ws_stream.split();
        let (write_tx, mut write_rx) = mpsc::channel::<String>(256);
        let pending: PendingRpcMap = Arc::new(Mutex::new(HashMap::new()));

        tokio::spawn(async move {
            while let Some(msg) = write_rx.recv().await {
                if sink.send(Message::Text(msg.into())).await.is_err() { break; }
            }
        });

        let pending_reader = Arc::clone(&pending);
        tokio::spawn(async move {
            while let Some(frame_res) = stream.next().await {
                let text = match frame_res {
                    Ok(Message::Text(t)) => t.to_string(),
                    Ok(Message::Binary(b)) => String::from_utf8_lossy(&b).into_owned(),
                    Ok(Message::Close(_)) => { let _ = push_tx.send(VmPushEvent::Disconnected("VM Service closed".into())); break; }
                    Ok(_) => continue,
                    Err(e) => { let _ = push_tx.send(VmPushEvent::Disconnected(format!("WS error: {e}"))); break; }
                };
                let Ok(val) = serde_json::from_str::<Value>(&text) else { continue };
                if let Some(id) = parse_rpc_id(val.get("id")) {
                    if let Some(tx) = pending_reader.lock().await.remove(&id) {
                        if let Some(err) = val.get("error") {
                            let msg = err.get("message").and_then(Value::as_str).unwrap_or("RPC error");
                            let details = err.get("data").and_then(|d| d.get("details")).and_then(Value::as_str);
                            let full_err = details.map_or_else(|| msg.to_string(), |d| format!("{msg}: {d}"));
                            let _ = tx.send(Err(DevToolsError::Rpc { method: format!("id:{id}"), message: full_err }));
                        } else {
                            let _ = tx.send(Ok(val.get("result").cloned().unwrap_or(Value::Null)));
                        }
                    }
                    continue;
                }
                if val.get("method").and_then(Value::as_str) == Some("streamNotify") && let Some(params) = val.get("params") {
                    handle_stream_notify(params, &push_tx, &log_seq);
                }
            }
            for (_, tx) in pending_reader.lock().await.drain() {
                let _ = tx.send(Err(DevToolsError::Connection("VM Service disconnected".into())));
            }
        });

        let client = Self {
            ws_uri, write_tx, pending, next_id: Arc::new(AtomicU64::new(1)),
            main_isolate_id: Arc::new(RwLock::new(String::new())),
            isolate_ids: Arc::new(RwLock::new(Vec::new())),
            extension_rpcs: Arc::new(RwLock::new(HashSet::new())),
            process_memory_supported: Arc::new(AtomicBool::new(true)),
        };
        let metadata = client.initialize_session().await?;
        Ok((client, metadata))
    }
    pub fn ws_uri(&self) -> &str { &self.ws_uri }
    pub async fn main_isolate_id(&self) -> String { self.main_isolate_id.read().await.clone() }
    pub async fn register_extension_rpc(&self, rpc: String) { self.extension_rpcs.write().await.insert(rpc); }
    pub async fn has_extension_rpc(&self, rpc: &str) -> bool {
        let rpcs = self.extension_rpcs.read().await;
        rpcs.is_empty() || rpcs.contains(rpc)
    }
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, DevToolsError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let payload = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": id.to_string(), "method": method, "params": params,
        }))
        .map_err(|e| DevToolsError::Protocol(e.to_string()))?;

        if self.write_tx.send(payload).await.is_err() {
            self.pending.lock().await.remove(&id);
            return Err(DevToolsError::Connection("VM Service write channel closed".into()));
        }
        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .map_err(|_| DevToolsError::Rpc { method: method.into(), message: "timed out".into() })?
            .map_err(|_| DevToolsError::Rpc { method: method.into(), message: "cancelled".into() })?
    }

    pub async fn call_isolate(&self, method: &str, params: Value) -> Result<Value, DevToolsError> {
        let isolate_id = self.main_isolate_id().await;
        if isolate_id.is_empty() {
            return Err(DevToolsError::Protocol("No active main isolate ID".into()));
        }
        self.call_specific_isolate(&isolate_id, method, params).await
    }

    pub async fn call_specific_isolate(
        &self,
        isolate_id: &str,
        method: &str,
        mut params: Value,
    ) -> Result<Value, DevToolsError> {
        if isolate_id.is_empty() {
            return Err(DevToolsError::Protocol("Empty isolate ID".into()));
        }
        if let Some(obj) = params.as_object_mut() {
            obj.insert("isolateId".to_string(), json!(isolate_id));
        } else {
            params = json!({ "isolateId": isolate_id });
        }
        self.call(method, params).await
    }

    async fn candidate_isolate_ids(&self, preferred: Option<&str>) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(pref) = preferred.filter(|s| !s.is_empty()) {
            out.push(pref.to_string());
        }
        let main_id = self.main_isolate_id().await;
        if !main_id.is_empty() && !out.contains(&main_id) {
            out.push(main_id);
        }
        for id in self.isolate_ids.read().await.iter() {
            if !id.is_empty() && !out.contains(id) {
                out.push(id.clone());
            }
        }
        out
    }

    async fn initialize_session(&self) -> Result<VmMetadata, DevToolsError> {
        let meta = self.refresh_main_isolate().await?;
        let streams = ["Logging", "Stdout", "Stderr", "Extension", "Isolate", "GC", "Debug"];
        let futs: Vec<_> = streams
            .into_iter()
            .map(|s| {
                let c = self.clone();
                tokio::spawn(async move { let _ = c.call("streamListen", json!({ "streamId": s })).await; })
            })
            .collect();
        for f in futs { let _ = f.await; }
        Ok(meta)
    }

    pub async fn refresh_main_isolate(&self) -> Result<VmMetadata, DevToolsError> {
        let vm = self.call("getVM", json!({})).await?;
        let target_cpu = vm.get("targetCPU").or_else(|| vm.get("hostCPU")).and_then(Value::as_str).unwrap_or("unknown").to_string();
        let arch_bits = vm.get("architectureBits").and_then(Value::as_u64).unwrap_or(64);
        let isolates = vm.get("isolates").and_then(Value::as_array)
            .ok_or_else(|| DevToolsError::Protocol("`getVM` returned no isolates".into()))?;

        let all_ids: Vec<String> = isolates
            .iter()
            .filter_map(|iso| iso.get("id").and_then(Value::as_str))
            .map(ToOwned::to_owned)
            .collect();
        *self.isolate_ids.write().await = all_ids.clone();

        let chosen = isolates
            .iter()
            .find(|iso| iso.get("name").and_then(Value::as_str).is_some_and(|n| n.contains("main")))
            .or_else(|| isolates.first())
            .ok_or_else(|| DevToolsError::Protocol("No running Dart isolate found".into()))?;

        let main_isolate_id = chosen.get("id").and_then(Value::as_str)
            .ok_or_else(|| DevToolsError::Protocol("Isolate missing `id`".into()))?.to_string();
        let main_isolate_name = chosen.get("name").and_then(Value::as_str).unwrap_or("main").to_string();

        *self.main_isolate_id.write().await = main_isolate_id.clone();
        let mut rpcs = HashSet::new();
        for iso_id in &all_ids {
            if let Ok(detail) = self.call("getIsolate", json!({ "isolateId": iso_id })).await
                && let Some(arr) = detail.get("extensionRPCs").and_then(Value::as_array)
            {
                for rpc in arr.iter().filter_map(Value::as_str) {
                    rpcs.insert(rpc.to_string());
                }
            }
        }
        *self.extension_rpcs.write().await = rpcs;
        self.set_http_timeline_logging(true).await;
        Ok(VmMetadata { main_isolate_name, target_cpu, arch_bits })
    }

    pub async fn set_http_timeline_logging(&self, enabled: bool) {
        let enabled_str = if enabled { "true" } else { "false" };
        for iso_id in self.candidate_isolate_ids(None).await {
            if self
                .call_specific_isolate(
                    &iso_id,
                    "ext.dart.io.httpEnableTimelineLogging",
                    json!({ "enabled": enabled_str }),
                )
                .await
                .is_err()
            {
                let _ = self
                    .call_specific_isolate(
                        &iso_id,
                        "httpEnableTimelineLogging",
                        json!({ "enabled": enabled }),
                    )
                    .await;
            }
        }
    }

    pub async fn poll_http_profile(&self, updated_since: Option<u64>) -> Result<(u64, Vec<HttpRequestRecord>), DevToolsError> {
        if !self.has_extension_rpc("ext.dart.io.getHttpProfile").await {
            return Ok((0, Vec::new()));
        }
        let mut str_params = json!({});
        let mut num_params = json!({});
        if let Some(ts) = updated_since {
            str_params["updatedSince"] = json!(ts.to_string());
            num_params["updatedSince"] = json!(ts);
        }

        let mut max_timestamp = 0u64;
        let mut all_requests = Vec::new();
        let mut any_ok = false;

        for iso_id in self.candidate_isolate_ids(None).await {
            let resp_res = match self
                .call_specific_isolate(&iso_id, "ext.dart.io.getHttpProfile", str_params.clone())
                .await
            {
                Ok(v) => Ok(v),
                Err(_) => {
                    self.call_specific_isolate(&iso_id, "getHttpProfile", num_params.clone())
                        .await
                }
            };
            if let Ok(resp) = resp_res {
                any_ok = true;
                let root = resp.get("result").filter(|r| r.is_object()).unwrap_or(&resp);
                let ts = root
                    .get("timestamp")
                    .and_then(|v| {
                        v.as_u64()
                            .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
                    })
                    .unwrap_or(0);
                max_timestamp = max_timestamp.max(ts);
                if let Some(arr) = root.get("requests").and_then(Value::as_array) {
                    for entry in arr {
                        if let Some(mut rec) = HttpRequestRecord::parse_from_profile_entry(entry) {
                            if rec.isolate_id.is_none() {
                                rec.isolate_id = Some(iso_id.clone());
                            }
                            all_requests.push(rec);
                        }
                    }
                }
            }
        }

        if !any_ok {
            return Ok((0, Vec::new()));
        }
        Ok((max_timestamp, all_requests))
    }

    pub async fn get_http_profile_request(
        &self,
        id: &str,
        preferred_isolate_id: Option<&str>,
    ) -> Result<HttpRequestDetail, DevToolsError> {
        let params = json!({ "id": id.to_string() });
        let candidates = self.candidate_isolate_ids(preferred_isolate_id).await;
        let mut last_err = None;

        for iso_id in candidates {
            match self
                .call_specific_isolate(&iso_id, "ext.dart.io.getHttpProfileRequest", params.clone())
                .await
            {
                Ok(resp) => return Ok(HttpRequestRecord::parse_detail_from_response(&resp)),
                Err(e1) => {
                    match self
                        .call_specific_isolate(&iso_id, "getHttpProfileRequest", params.clone())
                        .await
                    {
                        Ok(resp) => return Ok(HttpRequestRecord::parse_detail_from_response(&resp)),
                        Err(_) => last_err = Some(e1),
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| DevToolsError::Rpc {
            method: "ext.dart.io.getHttpProfileRequest".into(),
            message: format!("Unable to find HTTP profile request `{id}` in any active isolate"),
        }))
    }

    pub async fn clear_http_profile(&self) {
        for iso_id in self.candidate_isolate_ids(None).await {
            if self
                .call_specific_isolate(&iso_id, "ext.dart.io.clearHttpProfile", json!({}))
                .await
                .is_err()
            {
                let _ = self
                    .call_specific_isolate(&iso_id, "clearHttpProfile", json!({}))
                    .await;
            }
        }
    }

    pub async fn poll_websocket_profile(
        &self,
        updated_since: Option<u64>,
    ) -> Result<(u64, Vec<HttpRequestRecord>), DevToolsError> {
        if !self
            .has_extension_rpc("ext.dart.io.getWebSocketProfile")
            .await
        {
            return Ok((0, Vec::new()));
        }
        let mut params = json!({});
        if let Some(ts) = updated_since {
            params["updatedSince"] = json!(ts.to_string());
        }

        let mut max_timestamp = 0u64;
        let mut all_connections = Vec::new();
        let mut any_ok = false;

        for iso_id in self.candidate_isolate_ids(None).await {
            if let Ok(resp) = self
                .call_specific_isolate(&iso_id, "ext.dart.io.getWebSocketProfile", params.clone())
                .await
            {
                any_ok = true;
                let root = resp.get("result").filter(|r| r.is_object()).unwrap_or(&resp);
                let ts = root
                    .get("timestamp")
                    .and_then(|v| {
                        v.as_u64()
                            .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
                    })
                    .unwrap_or(0);
                max_timestamp = max_timestamp.max(ts);

                let arr = root
                    .get("connections")
                    .or_else(|| root.get("sockets"))
                    .or_else(|| root.get("webSockets"))
                    .or_else(|| root.get("requests"))
                    .and_then(Value::as_array);
                if let Some(entries) = arr {
                    for entry in entries {
                        if let Some(mut rec) =
                            HttpRequestRecord::parse_from_websocket_profile_entry(entry)
                        {
                            if rec.isolate_id.is_none() {
                                rec.isolate_id = Some(iso_id.clone());
                            }
                            if !rec.body_loaded
                                && let Ok(detail) = self
                                    .get_websocket_connection(&rec.id, rec.isolate_id.as_deref())
                                    .await
                            {
                                rec.apply_hydrated_websocket_detail(detail);
                            }
                            all_connections.push(rec);
                        }
                    }
                }
            }
        }

        if !any_ok {
            return Ok((0, Vec::new()));
        }
        Ok((max_timestamp, all_connections))
    }

    pub async fn get_websocket_connection(
        &self,
        id: &str,
        preferred_isolate_id: Option<&str>,
    ) -> Result<WebSocketConnectionDetail, DevToolsError> {
        let raw_id = id.strip_prefix("ws:").unwrap_or(id);
        let params = json!({ "id": raw_id });
        let candidates = self.candidate_isolate_ids(preferred_isolate_id).await;
        let mut last_err = None;

        for iso_id in candidates {
            match self
                .call_specific_isolate(
                    &iso_id,
                    "ext.dart.io.getWebSocketConnection",
                    params.clone(),
                )
                .await
            {
                Ok(resp) => return Ok(HttpRequestRecord::parse_websocket_detail_from_response(&resp)),
                Err(err) => last_err = Some(err),
            }
        }

        Err(last_err.unwrap_or_else(|| DevToolsError::Rpc {
            method: "ext.dart.io.getWebSocketConnection".into(),
            message: format!("Unable to find WebSocket connection `{raw_id}` in any active isolate"),
        }))
    }

    pub async fn clear_websocket_profile(&self) {
        if !self
            .has_extension_rpc("ext.dart.io.clearWebSocketProfile")
            .await
        {
            return;
        }
        for iso_id in self.candidate_isolate_ids(None).await {
            let _ = self
                .call_specific_isolate(&iso_id, "ext.dart.io.clearWebSocketProfile", json!({}))
                .await;
        }
    }

    pub async fn set_pub_root_directories(&self, project_root: &Path) {
        let params = build_pub_root_directories_params(project_root);
        if self
            .call_isolate("ext.flutter.inspector.addPubRootDirectories", params.clone())
            .await
            .is_err()
        {
            let _ = self
                .call_isolate("ext.flutter.inspector.setPubRootDirectories", params)
                .await;
        }
    }

    pub async fn dispose_inspector_group(&self, group_name: &str) {
        if !group_name.is_empty() {
            let _ = self.call_isolate("ext.flutter.inspector.disposeGroup", json!({ "objectGroup": group_name })).await;
        }
    }

    pub async fn fetch_widget_summary_tree(
        &self,
        group_name: &str,
        is_summary_tree: bool,
    ) -> Result<WidgetTreeNode, DevToolsError> {
        let is_summary_str = if is_summary_tree { "true" } else { "false" };
        let params = json!({
            "groupName": group_name,
            "isSummaryTree": is_summary_str,
            "withPreviews": "true",
            "fullDetails": "false"
        });
        let resp = match self
            .call_isolate("ext.flutter.inspector.getRootWidgetTree", params)
            .await
        {
            Ok(v) if !v.get("result").unwrap_or(&Value::Null).is_null() => v,
            _ => {
                let fallback_method = if is_summary_tree {
                    "ext.flutter.inspector.getRootWidgetSummaryTree"
                } else {
                    "ext.flutter.inspector.getRootWidget"
                };
                self.call_isolate(fallback_method, json!({ "objectGroup": group_name }))
                    .await?
            }
        };
        WidgetTreeNode::parse_from_json(&resp)
            .ok_or_else(|| DevToolsError::Protocol("Inspector returned empty widget tree".into()))
    }

    pub async fn fetch_widget_children(
        &self,
        value_id: &str,
        group_name: &str,
        is_summary_tree: bool,
    ) -> Result<Vec<WidgetTreeNode>, DevToolsError> {
        let method = if is_summary_tree {
            "ext.flutter.inspector.getChildrenSummaryTree"
        } else {
            "ext.flutter.inspector.getChildrenDetailsSubtree"
        };
        let resp = match self
            .call_isolate(method, json!({ "arg": value_id, "objectGroup": group_name }))
            .await
        {
            Ok(v) => v,
            Err(_) => {
                self.call_isolate(
                    "ext.flutter.inspector.getChildrenSummaryTree",
                    json!({ "arg": value_id, "objectGroup": group_name }),
                )
                .await?
            }
        };
        Ok(parse_widget_children_from_json(&resp))
    }

    pub async fn fetch_layout_explorer_node(
        &self,
        value_id: &str,
        group_name: &str,
    ) -> Result<LayoutExplorerNode, DevToolsError> {
        if value_id.is_empty() {
            return Err(DevToolsError::Protocol("Empty widget valueId".into()));
        }
        let resp = self
            .call_isolate(
                "ext.flutter.inspector.getLayoutExplorerNode",
                json!({
                    "id": value_id,
                    "arg": value_id,
                    "groupName": group_name,
                    "objectGroup": group_name,
                    "subtreeDepth": "1"
                }),
            )
            .await?;
        LayoutExplorerNode::parse_from_json(&resp).ok_or_else(|| {
            DevToolsError::Protocol("Failed to parse LayoutExplorerNode response".into())
        })
    }

    pub async fn fetch_details_subtree(
        &self,
        value_id: &str,
        group_name: &str,
    ) -> Result<WidgetTreeNode, DevToolsError> {
        if value_id.is_empty() {
            return Err(DevToolsError::Protocol("Empty widget valueId".into()));
        }
        let resp = self
            .call_isolate(
                "ext.flutter.inspector.getDetailsSubtree",
                json!({
                    "arg": value_id,
                    "objectGroup": group_name,
                    "subtreeDepth": "2"
                }),
            )
            .await?;
        WidgetTreeNode::parse_from_json(&resp).ok_or_else(|| {
            DevToolsError::Protocol("Failed to parse getDetailsSubtree response".into())
        })
    }

    pub async fn set_flex_properties(
        &self,
        value_id: &str,
        main_axis_alignment: &str,
        cross_axis_alignment: &str,
    ) -> Result<(), DevToolsError> {
        self.call_isolate(
            "ext.flutter.inspector.setFlexProperties",
            json!({
                "id": value_id,
                "mainAxisAlignment": qualify_enum("MainAxisAlignment", main_axis_alignment),
                "crossAxisAlignment": qualify_enum("CrossAxisAlignment", cross_axis_alignment)
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn set_flex_fit(&self, value_id: &str, flex_fit: &str) -> Result<(), DevToolsError> {
        self.call_isolate(
            "ext.flutter.inspector.setFlexFit",
            json!({
                "id": value_id,
                "flexFit": qualify_enum("FlexFit", flex_fit)
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn set_flex_factor(
        &self,
        value_id: &str,
        flex_factor: u32,
    ) -> Result<(), DevToolsError> {
        self.call_isolate(
            "ext.flutter.inspector.setFlexFactor",
            json!({
                "id": value_id,
                "flexFactor": format_flex_factor_param(flex_factor)
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn set_widget_selection_by_id(&self, value_id: &str, group_name: &str) {
        if !value_id.is_empty() {
            let _ = self.call_isolate("ext.flutter.inspector.setSelectionById", json!({ "arg": value_id, "objectGroup": group_name })).await;
        }
    }

    pub async fn get_selected_summary_widget(&self, group_name: &str) -> Result<WidgetTreeNode, DevToolsError> {
        if let Ok(resp) = self
            .call_isolate(
                "ext.flutter.inspector.getSelectedSummaryWidget",
                json!({ "objectGroup": group_name }),
            )
            .await
            && let Some(node) = WidgetTreeNode::parse_from_json(&resp)
        {
            return Ok(node);
        }
        let fallback_resp = self
            .call_isolate(
                "ext.flutter.inspector.getSelectedWidget",
                json!({ "objectGroup": group_name }),
            )
            .await?;
        WidgetTreeNode::parse_from_json(&fallback_resp)
            .ok_or_else(|| DevToolsError::Protocol("No selected widget".into()))
    }

    pub async fn set_isolate_exception_pause_mode(
        &self,
        mode: ExceptionPauseMode,
    ) -> Result<(), DevToolsError> {
        let mode_str = mode.vm_service_mode();
        let candidates = self.candidate_isolate_ids(None).await;
        if candidates.is_empty() {
            return Err(DevToolsError::Protocol("No active Dart isolate".into()));
        }
        let mut any_ok = false;
        for iso_id in candidates {
            if self
                .call_specific_isolate(
                    &iso_id,
                    "setIsolatePauseMode",
                    json!({ "exceptionPauseMode": mode_str }),
                )
                .await
                .is_ok()
                || self
                    .call_specific_isolate(
                        &iso_id,
                        "setExceptionPauseMode",
                        json!({ "mode": mode_str }),
                    )
                    .await
                    .is_ok()
            {
                any_ok = true;
            }
        }
        if any_ok {
            Ok(())
        } else {
            Err(DevToolsError::Rpc {
                method: "setIsolatePauseMode".into(),
                message: "VM Service rejected exceptionPauseMode update".into(),
            })
        }
    }

    pub async fn fetch_widget_properties(&self, value_id: &str, group_name: &str) -> Result<Vec<WidgetProperty>, DevToolsError> {
        if value_id.is_empty() { return Ok(Vec::new()); }
        let resp = self.call_isolate("ext.flutter.inspector.getProperties", json!({ "arg": value_id, "objectGroup": group_name })).await?;
        Ok(parse_widget_properties(&resp))
    }

    pub async fn poll_memory_usage(&self, is_gc_event: bool) -> Result<MemorySnapshot, DevToolsError> {
        let process_rss = if self.process_memory_supported.load(Ordering::Relaxed) {
            match self.call("getProcessMemoryUsage", json!({})).await {
                Ok(ref v) => parse_process_memory_rss(v),
                Err(_) => { self.process_memory_supported.store(false, Ordering::Relaxed); None }
            }
        } else { None };
        let mem = self.call_isolate("getMemoryUsage", json!({})).await?;
        MemorySnapshot::parse_memory_usage(&mem, process_rss, is_gc_event)
            .ok_or_else(|| DevToolsError::Protocol("Failed to parse getMemoryUsage".into()))
    }

    pub async fn trigger_force_gc(&self) -> Result<MemorySnapshot, DevToolsError> {
        self.call_isolate("getAllocationProfile", json!({ "gc": true })).await?;
        self.poll_memory_usage(true).await
    }

    pub async fn fetch_full_string_object(&self, object_id: &str) -> Option<String> {
        const MAX_CHARS: usize = 64 * 1024;
        let fut = self.call_isolate("getObject", json!({ "objectId": object_id }));
        let resp = tokio::time::timeout(Duration::from_secs(1), fut).await.ok()?.ok()?;
        let s = resp.get("valueAsString")?.as_str()?;
        Some(truncate_utf8_bytes(s, MAX_CHARS))
    }

    pub async fn query_initial_overlay_state(&self, base: &OverlayState) -> OverlayState {
        let mut out = base.clone();
        for overlay in ServiceOverlay::all() {
            let method = overlay.extension_method();
            if self.has_extension_rpc(method).await
                && let Ok(resp) = self.call_isolate(method, json!({})).await
                && let Some(val) = resp.get("enabled").or_else(|| resp.get("value")).or_else(|| resp.get("timeDilation")).and_then(Value::as_str)
            {
                out.sync_from_extension_event(method, val);
            }
        }
        out
    }
}

fn parse_rpc_id(val: Option<&Value>) -> Option<u64> {
    match val? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse::<u64>().ok(),
        _ => None,
    }
}

fn handle_stream_notify(params: &Value, push_tx: &mpsc::UnboundedSender<VmPushEvent>, log_seq: &AtomicU64) {
    let stream_id = params.get("streamId").and_then(Value::as_str).unwrap_or_default();
    let Some(event) = params.get("event") else { return };
    let kind = event.get("kind").and_then(Value::as_str).unwrap_or_default();

    match stream_id {
        "Stdout" | "Stderr" if kind == "WriteEvent" => {
            if let Some(b64) = event.get("bytes").and_then(Value::as_str) {
                for (log_kind, line) in LogEntry::decode_write_event_lines(stream_id, b64) {
                    let seq = log_seq.fetch_add(1, Ordering::Relaxed);
                    let _ = push_tx.send(VmPushEvent::Log(LogEntry::from_text(seq, log_kind, stream_id, line, None)));
                }
            }
        }
        "Logging" => {
            let seq = log_seq.fetch_add(1, Ordering::Relaxed);
            if let Some(parsed) = LogEntry::parse_logging_stream_record(event, seq) {
                let _ = push_tx.send(VmPushEvent::Log(parsed.entry.clone()));
                if parsed.truncated.has_any() {
                    let _ = push_tx.send(VmPushEvent::LogWithTruncatedRefs(parsed));
                }
            }
        }
        "GC" => {
            let reason = event.get("reason").and_then(Value::as_str).unwrap_or("collect");
            let new_used = event.get("new").and_then(|n| n.get("used")).and_then(Value::as_u64);
            let old_used = event.get("old").and_then(|o| o.get("used")).and_then(Value::as_u64);
            let detail = match (new_used, old_used) {
                (Some(n), Some(o)) => format!("Dart VM GC ({reason}) · new={}KB old={}KB", n / 1024, o / 1024),
                _ => format!("Dart VM Garbage Collection ({reason})"),
            };
            let seq = log_seq.fetch_add(1, Ordering::Relaxed);
            let _ = push_tx.send(VmPushEvent::Log(LogEntry::from_text(seq, LogKind::Gc, "GC", detail, None)));
            let _ = push_tx.send(VmPushEvent::GcNotification);
        }
        "Debug" if kind == "Inspect" => {
            let _ = push_tx.send(VmPushEvent::WidgetInspectEvent);
        }
        "Extension" => {
            let ext_kind = event.get("extensionKind").and_then(Value::as_str).unwrap_or_default();
            let ext_data = event.get("extensionData").unwrap_or(&Value::Null);
            match ext_kind {
                "Flutter.Frame" => {
                    if let Some(frame) = FrameTiming::parse_flutter_frame(ext_data) {
                        let _ = push_tx.send(VmPushEvent::Frame(frame));
                    }
                }
                "Flutter.Error" => {
                    let seq = log_seq.fetch_add(1, Ordering::Relaxed);
                    let _ = push_tx.send(VmPushEvent::Log(LogEntry::parse_flutter_error_extension(ext_data, seq)));
                }
                "Flutter.ServiceExtensionStateChanged" => {
                    if let Some(extension) = ext_data.get("extension").and_then(Value::as_str) {
                        let value = match ext_data.get("value") {
                            Some(Value::String(s)) => s.clone(),
                            Some(other) => other.to_string(),
                            None => String::new(),
                        };
                        let _ = push_tx.send(VmPushEvent::ServiceExtensionStateChanged { extension: extension.into(), value });
                    }
                }
                "Flutter.FirstFrame" | "Flutter.FrameworkInitialization" => {
                    let seq = log_seq.fetch_add(1, Ordering::Relaxed);
                    let _ = push_tx.send(VmPushEvent::Log(LogEntry::from_text(seq, LogKind::FlutterInfo, "Flutter", format!("Event: {ext_kind}"), None)));
                }
                _ => {}
            }
        }
        "Isolate" => match kind {
            "ServiceExtensionAdded" => {
                if let Some(rpc) = event.get("extensionRPC").and_then(Value::as_str) {
                    let _ = push_tx.send(VmPushEvent::ServiceExtensionAdded { rpc: rpc.into() });
                }
            }
            "IsolateStart" | "IsolateReload" | "IsolateRunnable" => {
                let _ = push_tx.send(VmPushEvent::IsolateReloaded);
            }
            _ => {}
        },
        _ => {}
    }
}

pub fn normalize_to_ws_uri(uri: &str) -> Result<String, DevToolsError> {
    let trimmed = uri.trim();
    if trimmed.is_empty() { return Err(DevToolsError::Connection("Empty VM Service URI".into())); }
    let with_scheme = if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        trimmed.to_string()
    } else {
        return Err(DevToolsError::Connection(format!("Unsupported VM Service URI scheme: `{trimmed}`")));
    };
    let mut parsed = url::Url::parse(&with_scheme).map_err(|e| DevToolsError::Connection(format!("Invalid VM Service URI: {e}")))?;
    let path = parsed.path().trim_end_matches('/').to_string();
    if !path.ends_with("/ws") { parsed.set_path(&format!("{path}/ws")); }
    Ok(parsed.to_string())
}

pub fn qualify_enum(prefix: &str, value: &str) -> String {
    let trimmed = value.trim();
    let expected_prefix = format!("{prefix}.");
    if trimmed.starts_with(&expected_prefix) {
        trimmed.to_string()
    } else {
        format!(
            "{prefix}.{}",
            trimmed.rsplit('.').next().unwrap_or(trimmed).trim()
        )
    }
}

pub fn format_flex_factor_param(flex_factor: u32) -> String {
    if flex_factor == 0 {
        "null".to_string()
    } else {
        flex_factor.to_string()
    }
}

pub fn truncate_utf8_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() > max_bytes {
        let end = s.floor_char_boundary(max_bytes);
        format!("{}... [truncated at 64KB]", &s[..end])
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalizes_http_and_ws_vm_service_uris() {
        assert_eq!(normalize_to_ws_uri("http://127.0.0.1:54321/abcdef=/").unwrap(), "ws://127.0.0.1:54321/abcdef=/ws");
        assert_eq!(normalize_to_ws_uri("http://127.0.0.1:54321/abcdef=/ws").unwrap(), "ws://127.0.0.1:54321/abcdef=/ws");
        assert_eq!(normalize_to_ws_uri("ws://localhost:8181").unwrap(), "ws://localhost:8181/ws");
        assert_eq!(normalize_to_ws_uri("https://127.0.0.1:9100/token").unwrap(), "wss://127.0.0.1:9100/token/ws");
        assert!(normalize_to_ws_uri("").is_err());
    }
    #[test]
    fn truncates_multibyte_utf8_safely_at_64kb_boundary() {
        let mut s = "a".repeat(65534);
        s.push('🦀'); // 4-byte emoji straddling 65536 (64KB)
        s.push_str("中文结尾");
        let truncated = truncate_utf8_bytes(&s, 64 * 1024);
        assert!(truncated.ends_with("... [truncated at 64KB]"));
        assert_eq!(truncated.len(), 65534 + "... [truncated at 64KB]".len());
    }
    #[test]
    fn qualifies_flutter_inspector_flex_enums_and_serializes_flex_factor() {
        assert_eq!(
            qualify_enum("MainAxisAlignment", "center"),
            "MainAxisAlignment.center"
        );
        assert_eq!(
            qualify_enum("MainAxisAlignment", "MainAxisAlignment.spaceBetween"),
            "MainAxisAlignment.spaceBetween"
        );
        assert_eq!(
            qualify_enum("CrossAxisAlignment", "stretch"),
            "CrossAxisAlignment.stretch"
        );
        assert_eq!(
            qualify_enum("CrossAxisAlignment", "CrossAxisAlignment.start"),
            "CrossAxisAlignment.start"
        );
        assert_eq!(qualify_enum("FlexFit", "tight"), "FlexFit.tight");
        assert_eq!(qualify_enum("FlexFit", "FlexFit.loose"), "FlexFit.loose");

        assert_eq!(format_flex_factor_param(0), "null");
        assert_eq!(format_flex_factor_param(1), "1");
        assert_eq!(format_flex_factor_param(3), "3");
    }
}
