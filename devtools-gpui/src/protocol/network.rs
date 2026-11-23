use serde_json::Value;

use crate::theme::format_micros_local_hms;

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
    /// `true` only after `ext.dart.io.getHttpProfileRequest` (or explicit `requestBody`/`responseBody`)
    /// has been fetched, distinguishing summary headers from full body payloads.
    pub body_loaded: bool,
    /// `true` only when `response.endTime` is present or an error occurred.
    pub response_complete: bool,
}

impl HttpRequestRecord {
    pub fn total_bytes(&self) -> u64 {
        if self.response_bytes > 0 { self.response_bytes } else { self.request_bytes }
    }

    pub fn is_pending(&self) -> bool {
        self.end_time_micros.is_none() && self.error.is_none() && self.status_code.is_none()
    }

    /// Parses an entry from `ext.dart.io.getHttpProfile` (`HttpProfileRequestRef` / `HttpProfileRequest`).
    /// Primarily reads official Dart SDK keys `"request"` and `"response"`, with fallback to
    /// legacy `"requestData"` / `"responseData"`.
    pub fn parse_from_profile_entry(v: &Value) -> Option<Self> {
        let id = match v.get("id") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => return None,
        };
        let method = v.get("method").and_then(Value::as_str).unwrap_or("GET").to_ascii_uppercase();
        let uri = v.get("uri").and_then(Value::as_str)?.to_string();
        let (host, path_and_query) = split_uri_host_and_path(&uri);

        let req_obj = v.get("request").or_else(|| v.get("requestData"));
        let resp_obj = v.get("response").or_else(|| v.get("responseData"));

        let start_time_micros = v
            .get("startTime")
            .and_then(Value::as_u64)
            .or_else(|| req_obj.and_then(|r| r.get("startTime")).and_then(Value::as_u64))
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
            .or_else(|| resp_obj.and_then(|r| r.get("mimeType")).and_then(Value::as_str).map(ToOwned::to_owned))
            .or_else(|| find_header_ci(&req_headers, "content-type"));

        let mime_short = shorten_mime_type(content_type.as_deref(), &path_and_query);
        let is_json = mime_short == "json"
            || content_type.as_deref().is_some_and(|ct| ct.to_ascii_lowercase().contains("json"))
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
                    let ts = ev.get("timestamp").and_then(Value::as_u64).unwrap_or(start_time_micros);
                    events.push(HttpTimingEvent { name: name.to_string(), timestamp_micros: ts });
                }
            }
        }

        let body_loaded = v.get("requestBody").is_some() || v.get("responseBody").is_some();
        let response_complete = end_time_micros.is_some() || error.is_some();
        let has_any_detail = body_loaded || !req_headers.is_empty() || !resp_headers.is_empty();
        let detail = has_any_detail.then(|| Self::parse_detail_from_response(v));

        let final_req_bytes = if request_bytes > 0 {
            request_bytes
        } else {
            detail.as_ref().and_then(|d| d.request_body.as_ref().map(|b| b.len() as u64)).unwrap_or(0)
        };
        let final_resp_bytes = if response_bytes > 0 {
            response_bytes
        } else {
            detail.as_ref().and_then(|d| d.response_body.as_ref().map(|b| b.len() as u64)).unwrap_or(0)
        };

        Some(Self {
            id,
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
            body_loaded,
            response_complete,
        })
    }

    pub fn parse_detail_from_response(v: &Value) -> HttpRequestDetail {
        let req_obj = v.get("request").or_else(|| v.get("requestData"));
        let resp_obj = v.get("response").or_else(|| v.get("responseData"));

        let request_headers = extract_headers_map(req_obj.and_then(|r| r.get("headers")));
        let response_headers = extract_headers_map(resp_obj.and_then(|r| r.get("headers")));
        let request_body = decode_body_field(v.get("requestBody"))
            .or_else(|| decode_body_field(req_obj.and_then(|r| r.get("body"))));
        let response_body = decode_body_field(v.get("responseBody"))
            .or_else(|| decode_body_field(resp_obj.and_then(|r| r.get("body"))));

        let conn_info = resp_obj
            .and_then(|r| r.get("connectionInfo"))
            .or_else(|| req_obj.and_then(|r| r.get("connectionInfo")));
        let remote_address = conn_info
            .and_then(|c| c.get("remoteAddress"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let local_port = conn_info.and_then(|c| c.get("localPort")).and_then(Value::as_u64);

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
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.clone())
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
fn decode_body_field(val: Option<&Value>) -> Option<String> {
    let val = val?;
    match val {
        Value::Array(arr) if !arr.is_empty() => {
            let bytes: Vec<u8> = arr
                .iter()
                .filter_map(Value::as_u64)
                .filter_map(|n| u8::try_from(n).ok())
                .collect();
            if bytes.is_empty() {
                return None;
            }
            match String::from_utf8(bytes) {
                Ok(s) if !s.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) => Some(s),
                Ok(s) => Some(format_binary_preview(s.as_bytes())),
                Err(err) => Some(format_binary_preview(err.as_bytes())),
            }
        }
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn format_binary_preview(bytes: &[u8]) -> String {
    let preview_len = bytes.len().min(64);
    let hex: Vec<String> = bytes[..preview_len].iter().map(|b| format!("{b:02x}")).collect();
    let suffix = if bytes.len() > preview_len { " ..." } else { "" };
    format!("<Binary payload: {} bytes>\n{}{}", bytes.len(), hex.join(" "), suffix)
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
        assert_eq!(detail.request_body.as_deref(), Some("{\"user\":\"flutter\"}"));
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
}
