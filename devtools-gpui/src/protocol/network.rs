use std::io::Read as _;

use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use serde_json::Value;

use crate::theme::{DevToolsTheme, format_bytes, format_duration_ms, format_micros_local_hms};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkProtocolKind {
    #[default]
    Http,
    WebSocket,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSocketEventRecord {
    pub timestamp_micros: u64,
    pub timestamp_hms: String,
    pub event_name: String,
    pub frame_number: Option<u64>,
    pub direction: Option<String>,
    pub opcode: Option<String>,
    pub payload_size: Option<u64>,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
    pub arguments_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WebSocketConnectionDetail {
    pub state: String,
    pub protocol: Option<String>,
    pub connect_timestamp: u64,
    pub open_timestamp: Option<u64>,
    pub close_timestamp: Option<u64>,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub frames_sent: u64,
    pub frames_received: u64,
    pub ping_count: u64,
    pub pong_count: u64,
    pub close_code: Option<u16>,
    pub close_reason: Option<String>,
    pub error: Option<String>,
    pub events: Vec<WebSocketEventRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpTimingEvent {
    pub name: String,
    pub timestamp_micros: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HttpRequestDetail {
    pub request_headers: Vec<(String, String)>,
    pub response_headers: Vec<(String, String)>,
    pub request_body: Option<String>,
    pub response_body: Option<String>,
    pub remote_address: Option<String>,
    pub local_port: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequestRecord {
    pub id: String,
    pub isolate_id: Option<String>,
    pub protocol_kind: NetworkProtocolKind,
    pub method: String,
    pub uri: String,
    pub path_and_query: String,
    pub host: String,
    pub status_code: Option<u16>,
    pub reason_phrase: Option<String>,
    pub content_type: Option<String>,
    pub mime_short: String,
    pub start_time_micros: u64,
    pub end_time_micros: Option<u64>,
    pub duration_ms: Option<u64>,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub timestamp_hms: String,
    pub is_json: bool,
    pub error: Option<String>,
    pub events: Vec<HttpTimingEvent>,
    pub detail: Option<HttpRequestDetail>,
    pub ws_detail: Option<WebSocketConnectionDetail>,
    /// `true` only after `ext.dart.io.getHttpProfileRequest` or `ext.dart.io.getWebSocketConnection`
    /// has been fetched, distinguishing summary entries from full payloads.
    pub body_loaded: bool,
    /// `true` only when `response.endTime` / `closeTimestamp` is present or an error occurred.
    pub response_complete: bool,
}

impl HttpRequestRecord {
    pub fn is_websocket(&self) -> bool {
        self.protocol_kind == NetworkProtocolKind::WebSocket
    }

    pub fn raw_ws_id(&self) -> &str {
        self.id.strip_prefix("ws:").unwrap_or(&self.id)
    }

    pub fn total_bytes(&self) -> u64 {
        if self.is_websocket() {
            self.request_bytes + self.response_bytes
        } else if self.response_bytes > 0 {
            self.response_bytes
        } else {
            self.request_bytes
        }
    }

    pub fn is_pending(&self) -> bool {
        if self.is_websocket() {
            let state = self
                .ws_detail
                .as_ref()
                .map(|d| d.state.as_str())
                .or(self.reason_phrase.as_deref())
                .unwrap_or("");
            matches!(
                state.to_ascii_lowercase().as_str(),
                "connecting" | "open" | "closing"
            )
        } else {
            self.end_time_micros.is_none() && self.error.is_none() && self.status_code.is_none()
        }
    }

    pub fn status_badge_label_and_color(&self) -> (String, gpui_kit::Rgba) {
        if self.is_websocket() {
            let state = self
                .ws_detail
                .as_ref()
                .map(|d| d.state.as_str())
                .or(self.reason_phrase.as_deref())
                .unwrap_or("open")
                .to_ascii_lowercase();
            if self.error.is_some() || state == "error" {
                ("ERR".to_string(), DevToolsTheme::error_red())
            } else if state == "open" {
                ("OPEN".to_string(), DevToolsTheme::success_green())
            } else if state == "connecting" || state == "closing" {
                ("CONN".to_string(), DevToolsTheme::warning_yellow())
            } else {
                let code_label = self
                    .ws_detail
                    .as_ref()
                    .and_then(|d| d.close_code)
                    .or(self.status_code)
                    .map_or_else(|| "CLOSED".to_string(), |c| c.to_string());
                (code_label, DevToolsTheme::text_secondary())
            }
        } else {
            let label = match self.status_code {
                Some(code) => code.to_string(),
                None if self.error.is_some() => "ERR".to_string(),
                None => "...".to_string(),
            };
            let color = if self.error.is_some() && self.status_code.is_none() {
                DevToolsTheme::error_red()
            } else {
                DevToolsTheme::status_color(self.status_code)
            };
            (label, color)
        }
    }

    pub fn duration_display(&self) -> String {
        if self.is_websocket() {
            let state = self
                .ws_detail
                .as_ref()
                .map(|d| d.state.as_str())
                .or(self.reason_phrase.as_deref())
                .unwrap_or("")
                .to_ascii_lowercase();
            if state == "open" || state == "connecting" {
                if let Some(ms) = self.duration_ms.filter(|&m| m > 0) {
                    return format_duration_ms(Some(ms));
                }
                return "Live".to_string();
            }
        }
        format_duration_ms(self.duration_ms)
    }

    /// Parses an entry from `ext.dart.io.getWebSocketProfile` or `ext.dart.io.getWebSocketConnection`.
    pub fn parse_from_websocket_profile_entry(raw_v: &Value) -> Option<Self> {
        let v = raw_v
            .get("result")
            .filter(|r| r.is_object())
            .unwrap_or(raw_v);
        let raw_id = match v.get("id") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => return None,
        };
        let id = if raw_id.starts_with("ws:") {
            raw_id
        } else {
            format!("ws:{raw_id}")
        };
        let isolate_id = v
            .get("isolateId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);

        let uri = v
            .get("url")
            .or_else(|| v.get("uri"))
            .and_then(Value::as_str)?
            .to_string();
        let (host, path_and_query) = split_uri_host_and_path(&uri);
        let mime_short = if uri.to_ascii_lowercase().starts_with("wss://") {
            "wss".to_string()
        } else {
            "ws".to_string()
        };

        let ws_detail = Self::parse_websocket_detail_from_response(v);
        let start_time_micros = ws_detail.connect_timestamp;
        let end_time_micros = ws_detail.close_timestamp;
        let duration_ms = end_time_micros
            .or_else(|| ws_detail.events.last().map(|e| e.timestamp_micros))
            .or(ws_detail.open_timestamp)
            .and_then(|end| end.checked_sub(start_time_micros))
            .map(|us| us / 1000);

        let state_lower = ws_detail.state.to_ascii_lowercase();
        let response_complete = matches!(state_lower.as_str(), "closed" | "error")
            || ws_detail.close_timestamp.is_some()
            || ws_detail.error.is_some();
        let has_events_array = v.get("events").is_some_and(Value::is_array);
        let is_full_type = v
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t == "WebSocketConnection" || t == "WebSocketProfileConnection");
        let body_loaded = has_events_array || is_full_type;

        let status_code = ws_detail.close_code.or(if state_lower == "open" {
            Some(101)
        } else {
            None
        });

        Some(Self {
            id,
            isolate_id,
            protocol_kind: NetworkProtocolKind::WebSocket,
            method: "WS".to_string(),
            uri,
            path_and_query,
            host,
            status_code,
            reason_phrase: Some(ws_detail.state.to_ascii_uppercase()),
            content_type: ws_detail.protocol.clone(),
            mime_short,
            start_time_micros,
            end_time_micros,
            duration_ms,
            request_bytes: ws_detail.bytes_sent,
            response_bytes: ws_detail.bytes_received,
            timestamp_hms: format_micros_local_hms(start_time_micros),
            is_json: false,
            error: ws_detail.error.clone(),
            events: Vec::new(),
            detail: None,
            ws_detail: Some(ws_detail),
            body_loaded,
            response_complete,
        })
    }

    pub fn parse_websocket_detail_from_response(raw_v: &Value) -> WebSocketConnectionDetail {
        let v = raw_v
            .get("result")
            .filter(|r| r.is_object())
            .unwrap_or(raw_v);
        let v = v
            .get("connection")
            .filter(|c| c.is_object())
            .unwrap_or(v);

        let state = v
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("open")
            .to_string();
        let protocol = v
            .get("protocol")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && *s != "null")
            .map(ToOwned::to_owned);

        let connect_timestamp = parse_u64_val(v.get("connectTimestamp"))
            .or_else(|| parse_u64_val(v.get("startTime")))
            .unwrap_or(0);
        let open_timestamp = parse_u64_val(v.get("openTimestamp"));
        let close_timestamp =
            parse_u64_val(v.get("closeTimestamp")).or_else(|| parse_u64_val(v.get("endTime")));

        let bytes_sent = parse_u64_val(v.get("bytesSent")).unwrap_or(0);
        let bytes_received = parse_u64_val(v.get("bytesReceived")).unwrap_or(0);
        let frames_sent = parse_u64_val(v.get("framesSent")).unwrap_or(0);
        let frames_received = parse_u64_val(v.get("framesReceived")).unwrap_or(0);
        let ping_count = parse_u64_val(v.get("pingCount")).unwrap_or(0);
        let pong_count = parse_u64_val(v.get("pongCount")).unwrap_or(0);

        let close_code = parse_u64_val(v.get("closeCode")).and_then(|c| u16::try_from(c).ok());
        let close_reason = v
            .get("closeReason")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && *s != "null")
            .map(ToOwned::to_owned);

        let error = match v.get("error") {
            Some(Value::String(s)) if !s.is_empty() && s != "null" => Some(s.clone()),
            Some(Value::Object(obj)) => {
                let et = obj.get("type").and_then(Value::as_str).unwrap_or("");
                let em = obj.get("message").and_then(Value::as_str).unwrap_or("");
                if !et.is_empty() && !em.is_empty() {
                    Some(format!("{et}: {em}"))
                } else if !em.is_empty() {
                    Some(em.to_string())
                } else if !et.is_empty() {
                    Some(et.to_string())
                } else {
                    None
                }
            }
            _ => None,
        };

        let mut events = Vec::new();
        if let Some(ev_arr) = v.get("events").and_then(Value::as_array) {
            for ev in ev_arr {
                let ts = parse_u64_val(ev.get("timestamp")).unwrap_or(connect_timestamp);
                let event_name = ev
                    .get("event")
                    .or_else(|| ev.get("name"))
                    .or_else(|| ev.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("WebSocket.Event")
                    .to_string();

                let args = ev.get("arguments").filter(|a| a.is_object()).unwrap_or(ev);
                let frame_number = parse_u64_val(args.get("frameNumber"))
                    .or_else(|| parse_u64_val(ev.get("frameNumber")));

                let direction = args
                    .get("direction")
                    .or_else(|| ev.get("direction"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .or_else(|| {
                        let lower = event_name.to_ascii_lowercase();
                        if lower.contains("send") || lower.contains("ping") {
                            Some("out".to_string())
                        } else if lower.contains("receive") || lower.contains("pong") {
                            Some("in".to_string())
                        } else {
                            None
                        }
                    });

                let opcode = args
                    .get("opcode")
                    .or_else(|| ev.get("opcode"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);

                let payload_size = parse_u64_val(args.get("payloadSize"))
                    .or_else(|| parse_u64_val(args.get("size")))
                    .or_else(|| parse_u64_val(args.get("bytes")))
                    .or_else(|| parse_u64_val(ev.get("payloadSize")));

                let error_type = args
                    .get("errorType")
                    .or_else(|| ev.get("errorType"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);

                let error_message = args
                    .get("errorMessage")
                    .or_else(|| ev.get("errorMessage"))
                    .or_else(|| args.get("error"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);

                let arguments_json = ev
                    .get("arguments")
                    .filter(|a| !a.is_null())
                    .and_then(|a| serde_json::to_string(a).ok());

                events.push(WebSocketEventRecord {
                    timestamp_micros: ts,
                    timestamp_hms: format_micros_local_hms(ts),
                    event_name,
                    frame_number,
                    direction,
                    opcode,
                    payload_size,
                    error_type,
                    error_message,
                    arguments_json,
                });
            }
        }

        WebSocketConnectionDetail {
            state,
            protocol,
            connect_timestamp,
            open_timestamp,
            close_timestamp,
            bytes_sent,
            bytes_received,
            frames_sent,
            frames_received,
            ping_count,
            pong_count,
            close_code,
            close_reason,
            error,
            events,
        }
    }

    /// Parses an entry from `ext.dart.io.getHttpProfile` (`HttpProfileRequestRef` / `HttpProfileRequest`).
    /// Primarily reads official Dart SDK keys `"request"` and `"response"`, with fallback to
    /// legacy `"requestData"` / `"responseData"`.
    pub fn parse_from_profile_entry(raw_v: &Value) -> Option<Self> {
        let v = raw_v
            .get("result")
            .filter(|r| r.is_object())
            .unwrap_or(raw_v);
        let id = match v.get("id") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => return None,
        };
        let isolate_id = v
            .get("isolateId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let method = v
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_ascii_uppercase();
        let uri = v.get("uri").and_then(Value::as_str)?.to_string();
        let (host, path_and_query) = split_uri_host_and_path(&uri);

        let req_obj = v.get("request").or_else(|| v.get("requestData"));
        let resp_obj = v.get("response").or_else(|| v.get("responseData"));

        let start_time_micros = v
            .get("startTime")
            .and_then(Value::as_u64)
            .or_else(|| {
                req_obj
                    .and_then(|r| r.get("startTime"))
                    .and_then(Value::as_u64)
            })
            .unwrap_or(0);

        let end_time_micros = resp_obj
            .and_then(|r| r.get("endTime"))
            .and_then(Value::as_u64);

        let duration_ms = end_time_micros
            .and_then(|end| end.checked_sub(start_time_micros))
            .map(|micros| micros / 1000);

        let status_code = resp_obj
            .and_then(|r| r.get("statusCode"))
            .or_else(|| v.get("statusCode"))
            .and_then(Value::as_u64)
            .and_then(|c| u16::try_from(c).ok());

        let reason_phrase = resp_obj
            .and_then(|r| r.get("reasonPhrase"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        let error = resp_obj
            .and_then(|r| r.get("error"))
            .and_then(Value::as_str)
            .or_else(|| req_obj.and_then(|r| r.get("error")).and_then(Value::as_str))
            .map(ToOwned::to_owned);

        let req_headers = extract_headers_map(req_obj.and_then(|r| r.get("headers")));
        let resp_headers = extract_headers_map(resp_obj.and_then(|r| r.get("headers")));

        let content_type = find_header_ci(&resp_headers, "content-type")
            .or_else(|| {
                resp_obj
                    .and_then(|r| r.get("mimeType"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .or_else(|| find_header_ci(&req_headers, "content-type"));

        let mime_short = shorten_mime_type(content_type.as_deref(), &path_and_query);
        let is_json = mime_short == "json"
            || content_type
                .as_deref()
                .is_some_and(|ct| ct.to_ascii_lowercase().contains("json"))
            || path_and_query.ends_with(".json");

        let request_bytes = req_obj
            .and_then(|r| r.get("contentLength"))
            .and_then(Value::as_i64)
            .filter(|&n| n > 0)
            .map_or(0, |n| n as u64);

        let response_bytes = resp_obj
            .and_then(|r| r.get("contentLength"))
            .and_then(Value::as_i64)
            .filter(|&n| n > 0)
            .map_or(0, |n| n as u64);

        let mut events = Vec::new();
        let events_arr = req_obj
            .and_then(|r| r.get("events"))
            .and_then(Value::as_array)
            .or_else(|| v.get("events").and_then(Value::as_array));
        if let Some(ev_arr) = events_arr {
            for ev in ev_arr {
                if let Some(name) = ev.get("event").and_then(Value::as_str) {
                    let ts = ev
                        .get("timestamp")
                        .and_then(Value::as_u64)
                        .unwrap_or(start_time_micros);
                    events.push(HttpTimingEvent {
                        name: name.to_string(),
                        timestamp_micros: ts,
                    });
                }
            }
        }

        let is_full_type = v.get("type").and_then(Value::as_str) == Some("HttpProfileRequest");
        let has_non_null_body = v.get("requestBody").is_some_and(|b| !b.is_null())
            || v.get("responseBody").is_some_and(|b| !b.is_null());
        let body_loaded = is_full_type || has_non_null_body;
        let response_complete = end_time_micros.is_some() || error.is_some();
        let has_any_detail = body_loaded || !req_headers.is_empty() || !resp_headers.is_empty();
        let detail = has_any_detail.then(|| Self::parse_detail_from_response(v));

        let final_req_bytes = if request_bytes > 0 {
            request_bytes
        } else {
            detail
                .as_ref()
                .and_then(|d| d.request_body.as_ref().map(|b| b.len() as u64))
                .unwrap_or(0)
        };
        let final_resp_bytes = if response_bytes > 0 {
            response_bytes
        } else {
            detail
                .as_ref()
                .and_then(|d| d.response_body.as_ref().map(|b| b.len() as u64))
                .unwrap_or(0)
        };

        Some(Self {
            id,
            isolate_id,
            protocol_kind: NetworkProtocolKind::Http,
            method,
            uri,
            path_and_query,
            host,
            status_code,
            reason_phrase,
            content_type,
            mime_short,
            start_time_micros,
            end_time_micros,
            duration_ms,
            request_bytes: final_req_bytes,
            response_bytes: final_resp_bytes,
            timestamp_hms: format_micros_local_hms(start_time_micros),
            is_json,
            error,
            events,
            detail,
            ws_detail: None,
            body_loaded,
            response_complete,
        })
    }

    pub fn parse_detail_from_response(raw_v: &Value) -> HttpRequestDetail {
        let v = raw_v
            .get("result")
            .filter(|r| r.is_object())
            .unwrap_or(raw_v);
        let req_obj = v.get("request").or_else(|| v.get("requestData"));
        let resp_obj = v.get("response").or_else(|| v.get("responseData"));

        let request_headers = extract_headers_map(req_obj.and_then(|r| r.get("headers")));
        let response_headers = extract_headers_map(resp_obj.and_then(|r| r.get("headers")));
        let req_encoding = find_header_ci(&request_headers, "content-encoding");
        let resp_encoding = find_header_ci(&response_headers, "content-encoding");

        let request_body =
            decode_body_field_with_encoding(v.get("requestBody"), req_encoding.as_deref()).or_else(
                || {
                    decode_body_field_with_encoding(
                        req_obj.and_then(|r| r.get("body")),
                        req_encoding.as_deref(),
                    )
                },
            );
        let response_body =
            decode_body_field_with_encoding(v.get("responseBody"), resp_encoding.as_deref())
                .or_else(|| {
                    decode_body_field_with_encoding(
                        resp_obj.and_then(|r| r.get("body")),
                        resp_encoding.as_deref(),
                    )
                });

        let conn_info = resp_obj
            .and_then(|r| r.get("connectionInfo"))
            .or_else(|| req_obj.and_then(|r| r.get("connectionInfo")));
        let remote_address = conn_info
            .and_then(|c| c.get("remoteAddress"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let local_port = conn_info
            .and_then(|c| c.get("localPort"))
            .and_then(Value::as_u64);

        HttpRequestDetail {
            request_headers,
            response_headers,
            request_body,
            response_body,
            remote_address,
            local_port,
        }
    }

    /// Generates a POSIX shell-safe `curl` command reproducing the HTTP request.
    pub fn to_curl_command(&self) -> String {
        let mut parts = vec![
            "curl".to_string(),
            "-X".to_string(),
            self.method.clone(),
            shell_single_quote(&self.uri),
        ];
        if let Some(detail) = &self.detail {
            for (k, v) in &detail.request_headers {
                if !k.eq_ignore_ascii_case("content-length") && !k.eq_ignore_ascii_case("host") {
                    parts.push("-H".to_string());
                    parts.push(shell_single_quote(&format!("{k}: {v}")));
                }
            }
            if let Some(body) = &detail.request_body
                && !body.is_empty()
                && !body.starts_with("<Binary payload:")
            {
                parts.push("--data-raw".to_string());
                parts.push(shell_single_quote(body));
            }
        }
        parts.join(" ")
    }

    /// Formats the complete HTTP request/response or WebSocket connection & frames
    /// into a structured plain-text report for one-click copying (`Copy All`).
    pub fn to_full_text_report(&self) -> String {
        if self.is_websocket() {
            let ws = self.ws_detail.clone().unwrap_or_default();
            let mut out = String::with_capacity(1024);
            out.push_str("=== WebSocket Connection ===\n");
            out.push_str(&format!("Connection ID: {}\n", self.raw_ws_id()));
            out.push_str(&format!("URL: {}\n", self.uri));
            out.push_str(&format!("State: {}\n", ws.state));
            out.push_str(&format!(
                "Protocol: {}\n",
                ws.protocol.as_deref().unwrap_or("-")
            ));
            out.push_str(&format!("Duration: {}\n", self.duration_display()));
            out.push_str(&format!("Connected At: {}\n", self.timestamp_hms));
            out.push_str(&format!(
                "Frames Sent / Received: {} / {}\n",
                ws.frames_sent, ws.frames_received
            ));
            out.push_str(&format!(
                "Bytes Sent / Received: {} / {} (Total: {})\n",
                format_bytes(ws.bytes_sent),
                format_bytes(ws.bytes_received),
                format_bytes(self.total_bytes())
            ));
            out.push_str(&format!(
                "Ping / Pong Count: {} / {}\n",
                ws.ping_count, ws.pong_count
            ));
            if let Some(code) = ws.close_code {
                out.push_str(&format!(
                    "Close Code: {} ({})\n",
                    code,
                    ws.close_reason.as_deref().unwrap_or("none")
                ));
            }
            if let Some(err) = ws.error.as_deref().or(self.error.as_deref()) {
                out.push_str(&format!("Error: {err}\n"));
            }

            out.push_str("\n=== WebSocket Events & Frames ===\n");
            if ws.events.is_empty() {
                out.push_str("<none>\n");
            } else {
                for ev in &ws.events {
                    let dir = ev.direction.as_deref().unwrap_or("-");
                    let frame = ev
                        .frame_number
                        .map_or_else(|| "-".to_string(), |n| format!("#{n}"));
                    let op = ev.opcode.as_deref().unwrap_or("-");
                    let sz = ev
                        .payload_size
                        .map_or_else(|| "-".to_string(), format_bytes);
                    out.push_str(&format!(
                        "[{}] {} (dir={}, frame={}, opcode={}, size={})",
                        ev.timestamp_hms, ev.event_name, dir, frame, op, sz
                    ));
                    if let Some(ref em) = ev.error_message {
                        out.push_str(&format!(" error={em}"));
                    }
                    if let Some(ref args) = ev.arguments_json {
                        out.push_str(&format!(" args={args}"));
                    }
                    out.push('\n');
                }
            }
            return out;
        }

        let status_text = match (self.status_code, self.reason_phrase.as_deref()) {
            (Some(code), Some(phrase)) if !phrase.is_empty() => format!("{code} {phrase}"),
            (Some(code), _) => code.to_string(),
            (None, _) if self.error.is_some() => "Error".to_string(),
            (None, _) => "Pending".to_string(),
        };
        let detail = self.detail.as_ref();

        let mut out = String::with_capacity(1024);
        out.push_str("=== General ===\n");
        out.push_str(&format!("Request Method: {}\n", self.method));
        out.push_str(&format!("Request URL: {}\n", self.uri));
        out.push_str(&format!("Status: {status_text}\n"));
        out.push_str(&format!(
            "Content-Type: {}\n",
            self.content_type.as_deref().unwrap_or("-")
        ));
        out.push_str(&format!("Duration: {}\n", format_duration_ms(self.duration_ms)));
        out.push_str(&format!("Timestamp: {}\n", self.timestamp_hms));
        out.push_str(&format!("Request Size: {}\n", format_bytes(self.request_bytes)));
        out.push_str(&format!("Response Size: {}\n", format_bytes(self.response_bytes)));
        if let Some(addr) = detail.and_then(|d| d.remote_address.as_deref()) {
            out.push_str(&format!("Remote Address: {addr}\n"));
        }
        if let Some(err) = self.error.as_deref() {
            out.push_str(&format!("Error: {err}\n"));
        }

        out.push_str("\n=== Request Headers ===\n");
        if let Some(d) = detail
            && !d.request_headers.is_empty()
        {
            for (k, v) in &d.request_headers {
                out.push_str(&format!("{k}: {v}\n"));
            }
        } else {
            out.push_str("<none>\n");
        }

        out.push_str("\n=== Request Body ===\n");
        match detail
            .and_then(|d| d.request_body.as_deref())
            .filter(|s| !s.is_empty())
        {
            Some(body) => {
                out.push_str(body);
                if !body.ends_with('\n') {
                    out.push('\n');
                }
            }
            None => out.push_str("<empty>\n"),
        }

        out.push_str("\n=== Response Headers ===\n");
        if let Some(d) = detail
            && !d.response_headers.is_empty()
        {
            for (k, v) in &d.response_headers {
                out.push_str(&format!("{k}: {v}\n"));
            }
        } else {
            out.push_str("<none>\n");
        }

        out.push_str("\n=== Response Body ===\n");
        match detail
            .and_then(|d| d.response_body.as_deref())
            .filter(|s| !s.is_empty())
        {
            Some(body) => {
                out.push_str(body);
                if !body.ends_with('\n') {
                    out.push('\n');
                }
            }
            None => out.push_str("<empty>\n"),
        }

        out
    }
}

fn parse_u64_val(val: Option<&Value>) -> Option<u64> {
    let v = val?;
    v.as_u64()
        .or_else(|| v.as_i64().filter(|&n| n >= 0).map(|n| n as u64))
        .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
}

fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn split_uri_host_and_path(uri: &str) -> (String, String) {
    if let Ok(parsed) = url::Url::parse(uri) {
        let host = parsed.host_str().unwrap_or("").to_string();
        let mut path = parsed.path().to_string();
        if path.is_empty() {
            path = "/".to_string();
        }
        if let Some(q) = parsed.query() {
            path.push('?');
            path.push_str(q);
        }
        (host, path)
    } else {
        (String::new(), uri.to_string())
    }
}

fn extract_headers_map(headers_val: Option<&Value>) -> Vec<(String, String)> {
    let Some(obj) = headers_val.and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(obj.len());
    for (k, v) in obj {
        let joined = match v {
            Value::String(s) => s.clone(),
            Value::Array(arr) => arr
                .iter()
                .map(|item| match item {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect::<Vec<_>>()
                .join(", "),
            other => other.to_string(),
        };
        out.push((k.clone(), joined));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn find_header_ci(headers: &[(String, String)], key: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.clone())
}

fn shorten_mime_type(content_type: Option<&str>, path: &str) -> String {
    if let Some(ct) = content_type {
        let lower = ct.to_ascii_lowercase();
        for (needle, label) in [
            ("json", "json"),
            ("html", "html"),
            ("protobuf", "grpc"),
            ("grpc", "grpc"),
            ("xml", "xml"),
            ("png", "img"),
            ("jpeg", "img"),
            ("webp", "img"),
            ("image", "img"),
            ("text/plain", "text"),
            ("form-urlencoded", "form"),
            ("multipart", "form"),
        ] {
            if lower.contains(needle) {
                return label.to_string();
            }
        }
    }
    if path.ends_with(".json") {
        "json".to_string()
    } else if path.ends_with(".png") || path.ends_with(".jpg") || path.ends_with(".webp") {
        "img".to_string()
    } else {
        "http".to_string()
    }
}

/// Decodes `requestBody` / `responseBody` from `ext.dart.io.getHttpProfileRequest`.
/// Official Dart VM Service protocol transmits bodies as `Vec<u8>` byte arrays (`Uint8List`).
/// Transparently decompresses `gzip` (by magic bytes `[0x1f, 0x8b]` or `Content-Encoding: gzip`)
/// and `deflate` (`Content-Encoding: deflate`) payloads before UTF-8 decoding.
#[cfg(test)]
fn decode_body_field(val: Option<&Value>) -> Option<String> {
    decode_body_field_with_encoding(val, None)
}

fn decode_body_field_with_encoding(
    val: Option<&Value>,
    content_encoding: Option<&str>,
) -> Option<String> {
    let val = val?;
    match val {
        Value::Array(arr) if !arr.is_empty() => {
            let raw_bytes: Vec<u8> = arr
                .iter()
                .filter_map(Value::as_u64)
                .filter_map(|n| u8::try_from(n).ok())
                .collect();
            if raw_bytes.is_empty() {
                return None;
            }
            let bytes = maybe_decompress_bytes(&raw_bytes, content_encoding);
            if bytes.contains(&0) {
                return Some(format_binary_preview(&bytes));
            }
            match String::from_utf8(bytes) {
                Ok(s) => {
                    let total_chars = s.chars().count().max(1);
                    let ctrl_chars = s
                        .chars()
                        .filter(|&c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
                        .count();
                    if ctrl_chars * 20 <= total_chars {
                        Some(s)
                    } else {
                        Some(format_binary_preview(s.as_bytes()))
                    }
                }
                Err(err) => Some(format_binary_preview(err.as_bytes())),
            }
        }
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn maybe_decompress_bytes(raw: &[u8], content_encoding: Option<&str>) -> Vec<u8> {
    let enc_lower = content_encoding
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let is_gzip_magic = raw.len() >= 2 && raw[0] == 0x1f && raw[1] == 0x8b;

    if is_gzip_magic || enc_lower.contains("gzip") {
        let mut decoder = GzDecoder::new(raw);
        let mut out = Vec::new();
        if decoder.read_to_end(&mut out).is_ok() && !out.is_empty() {
            return out;
        }
    }

    if enc_lower.contains("deflate") {
        let mut zlib = ZlibDecoder::new(raw);
        let mut out = Vec::new();
        if zlib.read_to_end(&mut out).is_ok() && !out.is_empty() {
            return out;
        }
        out.clear();
        let mut raw_deflate = DeflateDecoder::new(raw);
        if raw_deflate.read_to_end(&mut out).is_ok() && !out.is_empty() {
            return out;
        }
    }

    raw.to_vec()
}

fn format_binary_preview(bytes: &[u8]) -> String {
    let preview_len = bytes.len().min(64);
    let hex: Vec<String> = bytes[..preview_len]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let suffix = if bytes.len() > preview_len { " ..." } else { "" };
    format!(
        "<Binary payload: {} bytes>\n{}{}",
        bytes.len(),
        hex.join(" "),
        suffix
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_official_dart_io_http_profile_and_byte_array_body() {
        let summary_json = json!({
            "type": "HttpProfileRequestRef",
            "id": "1",
            "isolateId": "isolates/123",
            "method": "POST",
            "uri": "https://api.example.com/v1/auth?client=zed",
            "startTime": 1_700_000_000_000_000u64,
            "request": {
                "contentLength": 17,
                "headers": {
                    "content-type": ["application/json"],
                    "authorization": ["Bearer secret_token"]
                },
                "events": [
                    { "event": "Connection established", "timestamp": 1_700_000_000_020_000u64 },
                    { "event": "Request sent", "timestamp": 1_700_000_000_045_000u64 }
                ]
            },
            "response": {
                "statusCode": 201,
                "reasonPhrase": "Created",
                "contentLength": 15,
                "endTime": 1_700_000_000_125_000u64,
                "headers": { "content-type": ["application/json; charset=utf-8"] },
                "connectionInfo": { "remoteAddress": "93.184.216.34", "localPort": 54123 }
            }
        });

        let mut rec = HttpRequestRecord::parse_from_profile_entry(&summary_json).expect("parse");
        assert_eq!(rec.id, "1");
        assert_eq!(rec.method, "POST");
        assert_eq!(rec.status_code, Some(201));
        assert_eq!(rec.duration_ms, Some(125));
        assert!(!rec.is_pending());
        assert!(!rec.body_loaded);
        assert_eq!(rec.events.len(), 2);

        let full_json = json!({
            "type": "HttpProfileRequest",
            "id": "1",
            "method": "POST",
            "uri": "https://api.example.com/v1/auth?client=zed",
            "startTime": 1_700_000_000_000_000u64,
            "request": summary_json["request"],
            "response": summary_json["response"],
            "requestBody": b"{\"user\":\"flutter\"}".to_vec(),
            "responseBody": b"{\"status\":\"ok\"}".to_vec()
        });

        let detail = HttpRequestRecord::parse_detail_from_response(&full_json);
        assert_eq!(
            detail.request_body.as_deref(),
            Some("{\"user\":\"flutter\"}")
        );
        assert_eq!(detail.response_body.as_deref(), Some("{\"status\":\"ok\"}"));
        rec.detail = Some(detail);
        rec.body_loaded = true;
        let curl = rec.to_curl_command();
        assert!(curl.contains("curl -X POST 'https://api.example.com/v1/auth?client=zed'"));
        assert!(curl.contains("-H 'authorization: Bearer secret_token'"));
        assert!(curl.contains("--data-raw '{\"user\":\"flutter\"}'"));
    }

    #[test]
    fn plain_4_char_strings_are_never_corrupted_by_false_base64_decoding() {
        for plain in ["true", "test", "null", "data"] {
            let decoded = decode_body_field(Some(&Value::String(plain.to_string())));
            assert_eq!(decoded.as_deref(), Some(plain));
        }
    }

    #[test]
    fn decompresses_gzip_response_body_and_generates_full_text_report() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write as _;

        let raw_payload = r#"{"greeting":"hello from gzip","items":[1,2,3]}"#;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(raw_payload.as_bytes())
            .expect("write gz");
        let gz_bytes = encoder.finish().expect("finish gz");

        let full_json = json!({
            "type": "HttpProfileRequest",
            "id": "gz-42",
            "isolateId": "isolates/999",
            "method": "GET",
            "uri": "https://api.example.com/v1/compressed",
            "startTime": 1_700_000_000_000_000u64,
            "request": {
                "headers": { "accept-encoding": ["gzip"] }
            },
            "response": {
                "statusCode": 200,
                "reasonPhrase": "OK",
                "endTime": 1_700_000_000_050_000u64,
                "headers": {
                    "content-type": ["application/json"],
                    "content-encoding": ["gzip"]
                }
            },
            "requestBody": [],
            "responseBody": gz_bytes
        });

        let rec = HttpRequestRecord::parse_from_profile_entry(&full_json).expect("parse gz");
        assert_eq!(rec.isolate_id.as_deref(), Some("isolates/999"));
        assert!(rec.body_loaded);
        let detail = rec.detail.as_ref().expect("detail");
        assert_eq!(detail.response_body.as_deref(), Some(raw_payload));

        let report = rec.to_full_text_report();
        assert!(report.contains("=== General ==="));
        assert!(report.contains("Request URL: https://api.example.com/v1/compressed"));
        assert!(report.contains("=== Request Headers ===\naccept-encoding: gzip"));
        assert!(report.contains("=== Request Body ===\n<empty>"));
        assert!(report.contains("=== Response Body ===\n{\"greeting\":\"hello from gzip\""));
    }

    #[test]
    fn parses_dart_3_14_websocket_profile_and_connection_events() {
        let ws_conn_json = json!({
            "type": "WebSocketConnection",
            "id": "7",
            "isolateId": "isolates/101",
            "url": "wss://stream.example.com/v1/live?token=abc",
            "state": "closed",
            "protocol": "chat.v1",
            "connectTimestamp": 1_700_000_000_000_000u64,
            "openTimestamp": 1_700_000_000_030_000u64,
            "closeTimestamp": 1_700_000_001_530_000u64,
            "bytesSent": 128,
            "bytesReceived": 512,
            "framesSent": 2,
            "framesReceived": 3,
            "pingCount": 1,
            "pongCount": 1,
            "closeCode": 1000,
            "closeReason": "Normal closure",
            "events": [
                {
                    "timestamp": 1_700_000_000_000_000u64,
                    "event": "WebSocket.Connect",
                    "arguments": { "url": "wss://stream.example.com/v1/live?token=abc" }
                },
                {
                    "timestamp": 1_700_000_000_030_000u64,
                    "event": "WebSocket.Open",
                    "arguments": { "protocol": "chat.v1" }
                },
                {
                    "timestamp": 1_700_000_000_100_000u64,
                    "event": "WebSocket.Send",
                    "arguments": {
                        "frameNumber": 1,
                        "direction": "out",
                        "opcode": "TEXT",
                        "payloadSize": 64
                    }
                },
                {
                    "timestamp": 1_700_000_000_250_000u64,
                    "event": "WebSocket.Receive",
                    "arguments": {
                        "frameNumber": 2,
                        "direction": "in",
                        "opcode": "BINARY",
                        "payloadSize": 512
                    }
                },
                {
                    "timestamp": 1_700_000_001_530_000u64,
                    "event": "WebSocket.Close",
                    "arguments": { "closeCode": 1000, "closeReason": "Normal closure" }
                }
            ]
        });

        let rec =
            HttpRequestRecord::parse_from_websocket_profile_entry(&ws_conn_json).expect("parse ws");
        assert!(rec.is_websocket());
        assert_eq!(rec.id, "ws:7");
        assert_eq!(rec.raw_ws_id(), "7");
        assert_eq!(rec.method, "WS");
        assert_eq!(rec.mime_short, "wss");
        assert_eq!(rec.host, "stream.example.com");
        assert_eq!(rec.path_and_query, "/v1/live?token=abc");
        assert_eq!(rec.total_bytes(), 640);
        assert_eq!(rec.duration_ms, Some(1530));
        assert!(rec.body_loaded);
        assert!(rec.response_complete);
        let (badge, _) = rec.status_badge_label_and_color();
        assert_eq!(badge, "1000");

        let ws_detail = rec.ws_detail.as_ref().expect("ws_detail");
        assert_eq!(ws_detail.protocol.as_deref(), Some("chat.v1"));
        assert_eq!(ws_detail.frames_sent, 2);
        assert_eq!(ws_detail.frames_received, 3);
        assert_eq!(ws_detail.events.len(), 5);
        assert_eq!(ws_detail.events[2].direction.as_deref(), Some("out"));
        assert_eq!(ws_detail.events[2].opcode.as_deref(), Some("TEXT"));
        assert_eq!(ws_detail.events[2].payload_size, Some(64));
        assert_eq!(ws_detail.events[3].direction.as_deref(), Some("in"));
        assert_eq!(ws_detail.events[3].opcode.as_deref(), Some("BINARY"));

        let report = rec.to_full_text_report();
        assert!(report.contains("=== WebSocket Connection ==="));
        assert!(report.contains("Frames Sent / Received: 2 / 3"));
        assert!(report.contains("WebSocket.Send (dir=out, frame=#1, opcode=TEXT, size=64 B)"));
    }
}
