use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum DevToolsPage {
    Inspector,
    #[default]
    Network,
    Logging,
    Performance,
    Memory,
}

impl DevToolsPage {
    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "inspector" | "widget" | "widgets" => Some(Self::Inspector),
            "network" | "net" | "http" => Some(Self::Network),
            "logging" | "logs" | "log" => Some(Self::Logging),
            "performance" | "perf" | "frames" => Some(Self::Performance),
            "memory" | "mem" | "heap" => Some(Self::Memory),
            _ => None,
        }
    }

    pub fn as_slug(self) -> &'static str {
        match self {
            Self::Inspector => "inspector",
            Self::Network => "network",
            Self::Logging => "logging",
            Self::Performance => "performance",
            Self::Memory => "memory",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Inspector => "Inspector",
            Self::Network => "Network",
            Self::Logging => "Logging",
            Self::Performance => "Performance",
            Self::Memory => "Memory",
        }
    }

    pub const fn all() -> [Self; 5] {
        [
            Self::Inspector,
            Self::Network,
            Self::Logging,
            Self::Performance,
            Self::Memory,
        ]
    }
}

/// Supported Flutter Service Extension overlays & runtime toggles (1:1 compatible with `server/src/ipc.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceOverlay {
    Inspector,
    Performance,
    DebugPaint,
    Baselines,
    RepaintRainbow,
    SlowAnimations,
    DebugBanner,
    OversizedImages,
    PlatformOverride,
    BrightnessOverride,
}

impl ServiceOverlay {
    pub const fn all() -> [Self; 10] {
        [
            Self::Inspector,
            Self::Performance,
            Self::DebugPaint,
            Self::Baselines,
            Self::RepaintRainbow,
            Self::SlowAnimations,
            Self::DebugBanner,
            Self::OversizedImages,
            Self::PlatformOverride,
            Self::BrightnessOverride,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Inspector => "Widget Inspector",
            Self::Performance => "Performance Overlay",
            Self::DebugPaint => "Debug Paint",
            Self::Baselines => "Paint Baselines",
            Self::RepaintRainbow => "Repaint Rainbow",
            Self::SlowAnimations => "Slow Animations",
            Self::DebugBanner => "Debug Banner",
            Self::OversizedImages => "Oversized Images",
            Self::PlatformOverride => "Target Platform",
            Self::BrightnessOverride => "Brightness",
        }
    }

    pub fn extension_method(self) -> &'static str {
        match self {
            Self::Inspector => "ext.flutter.inspector.show",
            Self::Performance => "ext.flutter.showPerformanceOverlay",
            Self::DebugPaint => "ext.flutter.debugPaint",
            Self::Baselines => "ext.flutter.debugPaintBaselinesEnabled",
            Self::RepaintRainbow => "ext.flutter.repaintRainbow",
            Self::SlowAnimations => "ext.flutter.timeDilation",
            Self::DebugBanner => "ext.flutter.debugAllowBanner",
            Self::OversizedImages => "ext.flutter.invertOversizedImages",
            Self::PlatformOverride => "ext.flutter.platformOverride",
            Self::BrightnessOverride => "ext.flutter.brightnessOverride",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayState {
    pub inspector: bool,
    pub performance: bool,
    pub debug_paint: bool,
    pub baselines: bool,
    pub repaint_rainbow: bool,
    pub slow_animations: bool,
    pub debug_banner: bool,
    pub oversized_images: bool,
    pub platform_override: String,
    pub dark_brightness: bool,
}

impl Default for OverlayState {
    fn default() -> Self {
        Self {
            inspector: false,
            performance: false,
            debug_paint: false,
            baselines: false,
            repaint_rainbow: false,
            slow_animations: false,
            debug_banner: true,
            oversized_images: false,
            platform_override: "default".to_string(),
            dark_brightness: false,
        }
    }
}

impl OverlayState {
    pub fn is_enabled(&self, overlay: ServiceOverlay) -> bool {
        match overlay {
            ServiceOverlay::Inspector => self.inspector,
            ServiceOverlay::Performance => self.performance,
            ServiceOverlay::DebugPaint => self.debug_paint,
            ServiceOverlay::Baselines => self.baselines,
            ServiceOverlay::RepaintRainbow => self.repaint_rainbow,
            ServiceOverlay::SlowAnimations => self.slow_animations,
            ServiceOverlay::DebugBanner => self.debug_banner,
            ServiceOverlay::OversizedImages => self.oversized_images,
            ServiceOverlay::PlatformOverride => self.platform_override != "default",
            ServiceOverlay::BrightnessOverride => self.dark_brightness,
        }
    }

    pub fn badge_text(&self, overlay: ServiceOverlay) -> String {
        match overlay {
            ServiceOverlay::PlatformOverride => self.platform_override.clone(),
            ServiceOverlay::BrightnessOverride => {
                if self.dark_brightness {
                    "Dark".to_string()
                } else {
                    "Light".to_string()
                }
            }
            other => {
                if self.is_enabled(other) {
                    "ON".to_string()
                } else {
                    "OFF".to_string()
                }
            }
        }
    }

    pub fn next_toggle_params(&mut self, overlay: ServiceOverlay) -> (&'static str, Value) {
        let method = overlay.extension_method();
        match overlay {
            ServiceOverlay::Inspector => {
                self.inspector = !self.inspector;
                (method, json!({ "enabled": self.inspector.to_string() }))
            }
            ServiceOverlay::Performance => {
                self.performance = !self.performance;
                (method, json!({ "enabled": self.performance.to_string() }))
            }
            ServiceOverlay::DebugPaint => {
                self.debug_paint = !self.debug_paint;
                (method, json!({ "enabled": self.debug_paint.to_string() }))
            }
            ServiceOverlay::Baselines => {
                self.baselines = !self.baselines;
                (method, json!({ "enabled": self.baselines.to_string() }))
            }
            ServiceOverlay::RepaintRainbow => {
                self.repaint_rainbow = !self.repaint_rainbow;
                (
                    method,
                    json!({ "enabled": self.repaint_rainbow.to_string() }),
                )
            }
            ServiceOverlay::SlowAnimations => {
                self.slow_animations = !self.slow_animations;
                (
                    method,
                    json!({
                        "timeDilation": if self.slow_animations { "5.0" } else { "1.0" }
                    }),
                )
            }
            ServiceOverlay::DebugBanner => {
                self.debug_banner = !self.debug_banner;
                (method, json!({ "enabled": self.debug_banner.to_string() }))
            }
            ServiceOverlay::OversizedImages => {
                self.oversized_images = !self.oversized_images;
                (
                    method,
                    json!({ "enabled": self.oversized_images.to_string() }),
                )
            }
            ServiceOverlay::PlatformOverride => {
                const PLATFORMS: [&str; 5] = ["iOS", "android", "macOS", "linux", "windows"];
                let next = PLATFORMS
                    .iter()
                    .position(|p| *p == self.platform_override)
                    .map(|idx| PLATFORMS[(idx + 1) % PLATFORMS.len()])
                    .unwrap_or(PLATFORMS[0]);
                self.platform_override = next.to_string();
                (method, json!({ "value": next }))
            }
            ServiceOverlay::BrightnessOverride => {
                self.dark_brightness = !self.dark_brightness;
                let value = if self.dark_brightness {
                    "Brightness.dark"
                } else {
                    "Brightness.light"
                };
                (method, json!({ "value": value }))
            }
        }
    }

    pub fn sync_from_extension_event(&mut self, extension: &str, value: &str) {
        let truthy = value.eq_ignore_ascii_case("true");
        match extension {
            "ext.flutter.inspector.show" => self.inspector = truthy,
            "ext.flutter.showPerformanceOverlay" => self.performance = truthy,
            "ext.flutter.debugPaint" => self.debug_paint = truthy,
            "ext.flutter.debugPaintBaselinesEnabled" => self.baselines = truthy,
            "ext.flutter.repaintRainbow" => self.repaint_rainbow = truthy,
            "ext.flutter.timeDilation" => {
                self.slow_animations = value.parse::<f64>().is_ok_and(|v| v > 1.1);
            }
            "ext.flutter.debugAllowBanner" => self.debug_banner = truthy,
            "ext.flutter.invertOversizedImages" => self.oversized_images = truthy,
            "ext.flutter.platformOverride" => {
                if !value.is_empty() {
                    self.platform_override = value.to_string();
                }
            }
            "ext.flutter.brightnessOverride" => {
                self.dark_brightness = value.contains("dark");
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Network Profiler Models
// ---------------------------------------------------------------------------

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
}

impl HttpRequestRecord {
    pub fn total_bytes(&self) -> u64 {
        if self.response_bytes > 0 {
            self.response_bytes
        } else {
            self.request_bytes
        }
    }

    pub fn is_pending(&self) -> bool {
        self.end_time_micros.is_none() && self.status_code.is_none() && self.error.is_none()
    }

    pub fn parse_from_profile_entry(v: &Value) -> Option<Self> {
        let id = match v.get("id") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => return None,
        };
        let method = v
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_ascii_uppercase();
        let uri = v.get("uri").and_then(Value::as_str)?.to_string();

        let (host, path_and_query) = split_uri_host_and_path(&uri);

        let start_time_micros = v.get("startTime").and_then(Value::as_u64).unwrap_or(0);
        let end_time_micros = v.get("endTime").and_then(Value::as_u64);
        let duration_ms = end_time_micros
            .and_then(|end| end.checked_sub(start_time_micros))
            .map(|micros| micros / 1000);

        let req_data = v.get("requestData");
        let resp_data = v.get("responseData");

        let status_code = resp_data
            .and_then(|r| r.get("statusCode"))
            .and_then(Value::as_u64)
            .and_then(|c| u16::try_from(c).ok())
            .or_else(|| {
                v.get("statusCode")
                    .and_then(Value::as_u64)
                    .and_then(|c| u16::try_from(c).ok())
            });

        let reason_phrase = resp_data
            .and_then(|r| r.get("reasonPhrase"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        let error = resp_data
            .and_then(|r| r.get("error"))
            .and_then(Value::as_str)
            .or_else(|| req_data.and_then(|r| r.get("error")).and_then(Value::as_str))
            .map(ToOwned::to_owned);

        let req_headers = extract_headers_map(req_data.and_then(|r| r.get("headers")));
        let resp_headers = extract_headers_map(resp_data.and_then(|r| r.get("headers")));

        let content_type = find_header_ci(&resp_headers, "content-type")
            .or_else(|| {
                resp_data
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

        let request_bytes = req_data
            .and_then(|r| r.get("contentLength"))
            .and_then(Value::as_i64)
            .filter(|&n| n > 0)
            .map(|n| n as u64)
            .unwrap_or(0);

        let response_bytes = resp_data
            .and_then(|r| r.get("contentLength"))
            .and_then(Value::as_i64)
            .filter(|&n| n > 0)
            .map(|n| n as u64)
            .unwrap_or(0);

        let mut events = Vec::new();
        if let Some(ev_arr) = v.get("events").and_then(Value::as_array) {
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

        // If full request/response details are embedded directly in this payload, parse them now
        let has_inline_detail = v.get("requestBody").is_some()
            || v.get("responseBody").is_some()
            || !req_headers.is_empty()
            || !resp_headers.is_empty();
        let detail = if has_inline_detail {
            Some(Self::parse_detail_from_response(v))
        } else {
            None
        };

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
            timestamp_hms: format_micros_to_hms(start_time_micros),
            is_json,
            error,
            events,
            detail,
        })
    }

    pub fn parse_detail_from_response(v: &Value) -> HttpRequestDetail {
        let req_data = v.get("requestData");
        let resp_data = v.get("responseData");

        let request_headers = extract_headers_map(req_data.and_then(|r| r.get("headers")));
        let response_headers = extract_headers_map(resp_data.and_then(|r| r.get("headers")));

        let request_body = decode_body_field(v.get("requestBody"))
            .or_else(|| decode_body_field(req_data.and_then(|r| r.get("body"))));
        let response_body = decode_body_field(v.get("responseBody"))
            .or_else(|| decode_body_field(resp_data.and_then(|r| r.get("body"))));

        let conn_info = resp_data
            .and_then(|r| r.get("connectionInfo"))
            .or_else(|| req_data.and_then(|r| r.get("connectionInfo")));
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
                if k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("host") {
                    continue;
                }
                parts.push("-H".to_string());
                parts.push(shell_single_quote(&format!("{k}: {v}")));
            }
            if let Some(body) = &detail.request_body
                && !body.is_empty()
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
    let Some(val) = headers_val else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(obj) = val.as_object() {
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
        if lower.contains("json") {
            return "json".to_string();
        }
        if lower.contains("html") {
            return "html".to_string();
        }
        if lower.contains("protobuf") || lower.contains("grpc") {
            return "grpc".to_string();
        }
        if lower.contains("xml") {
            return "xml".to_string();
        }
        if lower.contains("png") || lower.contains("jpeg") || lower.contains("webp") || lower.contains("image") {
            return "img".to_string();
        }
        if lower.contains("text/plain") {
            return "text".to_string();
        }
        if lower.contains("form-urlencoded") || lower.contains("multipart") {
            return "form".to_string();
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

fn decode_body_field(val: Option<&Value>) -> Option<String> {
    let val = val?;
    match val {
        Value::String(s) if !s.is_empty() => {
            // Dart VM getHttpProfileRequest sometimes returns base64 or plain text
            if let Some(decoded) = try_decode_base64_utf8(s) {
                Some(decoded)
            } else {
                Some(s.clone())
            }
        }
        Value::Array(arr) if !arr.is_empty() => {
            let bytes: Vec<u8> = arr
                .iter()
                .filter_map(Value::as_u64)
                .filter_map(|b| u8::try_from(b).ok())
                .collect();
            if bytes.is_empty() {
                None
            } else {
                Some(String::from_utf8_lossy(&bytes).into_owned())
            }
        }
        _ => None,
    }
}

fn try_decode_base64_utf8(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('{')
        || trimmed.starts_with('[')
        || trimmed.starts_with('<')
        || trimmed.contains(' ')
        || !trimmed.len().is_multiple_of(4)
    {
        return None;
    }

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
            b'=' => continue,
            _ => return None,
        };
        buf = (buf << 6) | (val as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }

    let s = String::from_utf8(out).ok()?;
    if s.chars().all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t')) {
        Some(s)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Flutter Widget Inspector Models
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLocation {
    pub uri: String,
    pub line: u32,
    pub column: u32,
}

impl SourceLocation {
    pub fn short_label(&self) -> String {
        let file_name = self
            .uri
            .rsplit('/')
            .next()
            .unwrap_or(&self.uri);
        format!("{file_name}:{}", self.line)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetProperty {
    pub name: String,
    pub value: String,
    pub level: String,
    pub property_type: Option<String>,
    pub color_argb: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetTreeNode {
    pub value_id: String,
    pub widget_runtime_type: String,
    pub description: String,
    pub has_children: bool,
    pub created_by_local_project: bool,
    pub location: Option<SourceLocation>,
    pub properties: Vec<WidgetProperty>,
    pub children: Vec<WidgetTreeNode>,
    pub expanded: bool,
}

impl WidgetTreeNode {
    pub fn parse_from_json(v: &Value) -> Option<Self> {
        let root = v.get("result").unwrap_or(v);
        if root.is_null() {
            return None;
        }
        Self::parse_node(root, 0)
    }

    fn parse_node(node: &Value, depth: usize) -> Option<Self> {
        let description = node
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("Widget")
            .to_string();
        let widget_runtime_type = node
            .get("widgetRuntimeType")
            .and_then(Value::as_str)
            .unwrap_or(&description)
            .to_string();
        let value_id = node
            .get("valueId")
            .and_then(Value::as_str)
            .or_else(|| node.get("objectId").and_then(Value::as_str))
            .unwrap_or("")
            .to_string();

        let created_by_local_project = node
            .get("createdByLocalProject")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let location = parse_creation_location(node.get("creationLocation"));

        let children: Vec<WidgetTreeNode> = node
            .get("children")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|child| Self::parse_node(child, depth + 1))
                    .collect()
            })
            .unwrap_or_default();

        let has_children = node
            .get("hasChildren")
            .and_then(Value::as_bool)
            .unwrap_or(!children.is_empty());

        let properties = parse_widget_properties(node);

        Some(Self {
            value_id,
            widget_runtime_type,
            description,
            has_children,
            created_by_local_project,
            location,
            properties,
            children,
            expanded: depth < 3 || created_by_local_project,
        })
    }
}

fn parse_creation_location(loc: Option<&Value>) -> Option<SourceLocation> {
    let loc = loc?;
    let uri = loc
        .get("file")
        .and_then(Value::as_str)
        .or_else(|| loc.get("uri").and_then(Value::as_str))?
        .to_string();
    let line = loc
        .get("line")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(1);
    let column = loc
        .get("column")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(1);
    Some(SourceLocation { uri, line, column })
}

pub fn parse_widget_properties(v: &Value) -> Vec<WidgetProperty> {
    let root = v.get("result").unwrap_or(v);
    let arr = root
        .get("properties")
        .and_then(Value::as_array)
        .or_else(|| root.as_array());

    let Some(items) = arr else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for item in items {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let description = item
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if name.is_empty() && description.is_empty() {
            continue;
        }
        let level = item
            .get("level")
            .and_then(Value::as_str)
            .unwrap_or("info")
            .to_string();
        if level == "hidden" {
            continue;
        }
        let property_type = item
            .get("propertyType")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        let color_argb = extract_color_argb(item, &description);

        out.push(WidgetProperty {
            name: if name.is_empty() {
                "value".to_string()
            } else {
                name
            },
            value: description,
            level,
            property_type,
            color_argb,
        });
    }
    out
}

fn extract_color_argb(item: &Value, description: &str) -> Option<u32> {
    if let Some(props) = item.get("valueProperties").and_then(Value::as_object) {
        let r = props.get("red")?.as_u64()? as u32;
        let g = props.get("green")?.as_u64()? as u32;
        let b = props.get("blue")?.as_u64()? as u32;
        let a = props.get("alpha").and_then(Value::as_u64).unwrap_or(255) as u32;
        return Some(((a & 0xFF) << 24) | ((r & 0xFF) << 16) | ((g & 0xFF) << 8) | (b & 0xFF));
    }
    // Check `Color(0xff123456)` in description
    if let Some(idx) = description.find("0x") {
        let hex: String = description[idx + 2..]
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .collect();
        if hex.len() == 8 {
            return u32::from_str_radix(&hex, 16).ok();
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Logging Stream Models
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    Stdout,
    Stderr,
    Log,
    FlutterError,
    FlutterInfo,
    Gc,
}

impl LogKind {
    pub fn badge_label(self) -> &'static str {
        match self {
            Self::Stdout => "STDOUT",
            Self::Stderr => "STDERR",
            Self::Log => "LOG",
            Self::FlutterError => "ERROR",
            Self::FlutterInfo => "FLUTTER",
            Self::Gc => "GC",
        }
    }

    pub fn is_error(self) -> bool {
        matches!(self, Self::Stderr | Self::FlutterError)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub seq: u64,
    pub timestamp_hms: String,
    pub kind: LogKind,
    pub logger_name: String,
    pub summary: String,
    pub full_text: String,
    pub stack_trace: Option<String>,
    pub source_refs: Vec<SourceLocation>,
}

impl LogEntry {
    pub fn from_text(seq: u64, kind: LogKind, logger_name: &str, text: String, stack: Option<String>) -> Self {
        let summary = text
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string();
        let mut source_refs = extract_source_locations_from_text(&text);
        if let Some(ref st) = stack {
            for loc in extract_source_locations_from_text(st) {
                if !source_refs.contains(&loc) {
                    source_refs.push(loc);
                }
            }
        }
        Self {
            seq,
            timestamp_hms: current_hms_string(),
            kind,
            logger_name: logger_name.to_string(),
            summary: if summary.is_empty() {
                text.trim().to_string()
            } else {
                summary
            },
            full_text: text,
            stack_trace: stack,
            source_refs,
        }
    }

    pub fn parse_logging_stream_event(event: &Value, seq: u64) -> Option<Self> {
        let rec = event.get("logRecord")?;
        let message = rec
            .get("message")
            .and_then(|m| m.get("valueAsString"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if message.is_empty() {
            return None;
        }
        let logger_name = rec
            .get("loggerName")
            .and_then(|m| m.get("valueAsString"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("dart:developer")
            .to_string();
        let level = rec.get("level").and_then(Value::as_i64).unwrap_or(0);
        let error_str = rec
            .get("error")
            .and_then(|m| m.get("valueAsString"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && *s != "null")
            .map(ToOwned::to_owned);
        let stack_str = rec
            .get("stackTrace")
            .and_then(|m| m.get("valueAsString"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && *s != "null")
            .map(ToOwned::to_owned);

        let kind = if level >= 1000 || error_str.is_some() {
            LogKind::FlutterError
        } else {
            LogKind::Log
        };

        let full_text = match error_str {
            Some(err) => format!("{message}\nError: {err}"),
            None => message,
        };
        Some(Self::from_text(seq, kind, &logger_name, full_text, stack_str))
    }

    pub fn parse_flutter_error_extension(ext_data: &Value, seq: u64) -> Self {
        let desc = ext_data
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("Flutter framework error")
            .to_string();
        let stack = ext_data
            .get("stackTrace")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        Self::from_text(seq, LogKind::FlutterError, "Flutter.Error", desc, stack)
    }
}

/// Extracts `package:foo/bar.dart:12:34` or `file:///path/to/bar.dart:12:34` references from log/stack lines.
pub fn extract_source_locations_from_text(text: &str) -> Vec<SourceLocation> {
    let mut locations = Vec::new();
    for token in text.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | ',')) {
        let trimmed = token.trim_matches(|c: char| matches!(c, '\'' | '"' | ';'));
        if (trimmed.starts_with("package:") || trimmed.starts_with("file://"))
            && let Some(loc) = parse_dart_uri_with_line_col(trimmed)
            && !locations.contains(&loc)
        {
            locations.push(loc);
        }
    }
    locations
}

pub fn parse_dart_uri_with_line_col(token: &str) -> Option<SourceLocation> {
    let dart_idx = token.find(".dart")?;
    let uri_end = dart_idx + ".dart".len();
    let uri = token[..uri_end].to_string();
    let tail = &token[uri_end..];
    let mut nums = tail.trim_start_matches(':').split(':');
    let line = nums.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(1);
    let column = nums.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(1);
    Some(SourceLocation { uri, line, column })
}

// ---------------------------------------------------------------------------
// Performance Frame & Memory Models
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTiming {
    pub number: u64,
    pub build_micros: u64,
    pub raster_micros: u64,
    pub total_micros: u64,
    pub vsync_overhead_micros: u64,
}

impl FrameTiming {
    pub fn parse_flutter_frame(ext_data: &Value) -> Option<Self> {
        let number = ext_data.get("number").and_then(Value::as_u64)?;
        let build_micros = ext_data.get("build").and_then(Value::as_u64).unwrap_or(0);
        let raster_micros = ext_data.get("raster").and_then(Value::as_u64).unwrap_or(0);
        let elapsed = ext_data
            .get("elapsed")
            .and_then(Value::as_u64)
            .unwrap_or(build_micros + raster_micros);
        let vsync_overhead_micros = ext_data
            .get("vsyncOverhead")
            .and_then(Value::as_u64)
            .unwrap_or(0);

        Some(Self {
            number,
            build_micros,
            raster_micros,
            total_micros: elapsed.max(build_micros + raster_micros),
            vsync_overhead_micros,
        })
    }

    pub fn build_ms(self) -> f64 {
        (self.build_micros as f64) / 1000.0
    }

    pub fn raster_ms(self) -> f64 {
        (self.raster_micros as f64) / 1000.0
    }

    pub fn total_ms(self) -> f64 {
        (self.total_micros as f64) / 1000.0
    }

    pub fn is_jank(self, budget_ms: f64) -> bool {
        self.build_ms() > budget_ms || self.raster_ms() > budget_ms || self.total_ms() > budget_ms * 1.5
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySnapshot {
    pub timestamp_hms: String,
    pub heap_usage_bytes: u64,
    pub heap_capacity_bytes: u64,
    pub external_usage_bytes: u64,
    pub rss_bytes: u64,
    pub is_gc_event: bool,
}

impl MemorySnapshot {
    pub fn parse_memory_usage(v: &Value, rss_bytes: u64, is_gc_event: bool) -> Option<Self> {
        let root = v.get("result").unwrap_or(v);
        let heap_usage_bytes = root.get("heapUsage").and_then(Value::as_u64)?;
        let heap_capacity_bytes = root
            .get("heapCapacity")
            .and_then(Value::as_u64)
            .unwrap_or(heap_usage_bytes);
        let external_usage_bytes = root
            .get("externalUsage")
            .and_then(Value::as_u64)
            .unwrap_or(0);

        Some(Self {
            timestamp_hms: current_hms_string(),
            heap_usage_bytes,
            heap_capacity_bytes,
            external_usage_bytes,
            rss_bytes: if rss_bytes > 0 {
                rss_bytes
            } else {
                heap_capacity_bytes + external_usage_bytes
            },
            is_gc_event,
        })
    }
}

// ---------------------------------------------------------------------------
// JSON Syntax Tokenizer for Formatted Code View
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonTokenKind {
    Punctuation,
    Key,
    StringVal,
    NumberVal,
    BoolOrNull,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonHighlightedLine {
    pub spans: Vec<(JsonTokenKind, String)>,
}

/// Pretty-prints a raw string if it is valid JSON and tokenizes each line for syntax-colored rendering.
pub fn format_and_tokenize_json(raw: &str) -> (String, Vec<JsonHighlightedLine>) {
    let pretty = match serde_json::from_str::<Value>(raw) {
        Ok(val) => serde_json::to_string_pretty(&val).unwrap_or_else(|_| raw.to_string()),
        Err(_) => raw.to_string(),
    };

    let lines = pretty
        .lines()
        .map(tokenize_pretty_json_line)
        .collect::<Vec<_>>();
    (pretty, lines)
}

fn tokenize_pretty_json_line(line: &str) -> JsonHighlightedLine {
    let trimmed = line.trim_start();
    let indent_len = line.len() - trimmed.len();
    let indent = &line[..indent_len];

    // Match `"key": value`
    if let Some(after_quote) = trimmed.strip_prefix('"')
        && let Some(end_quote_idx) = find_unescaped_quote(after_quote)
    {
        let key_part = &trimmed[..end_quote_idx + 2];
        let rest = &trimmed[end_quote_idx + 2..];
        if let Some(after_colon) = rest.strip_prefix(": ") {
            let mut spans = vec![
                (JsonTokenKind::Punctuation, indent.to_string()),
                (JsonTokenKind::Key, key_part.to_string()),
                (JsonTokenKind::Punctuation, ": ".to_string()),
            ];
            spans.extend(tokenize_json_value_tail(after_colon));
            return JsonHighlightedLine { spans };
        }
    }

    let mut spans = Vec::new();
    if !indent.is_empty() {
        spans.push((JsonTokenKind::Punctuation, indent.to_string()));
    }
    spans.extend(tokenize_json_value_tail(trimmed));
    JsonHighlightedLine { spans }
}

fn find_unescaped_quote(s: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, ch) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            return Some(i);
        }
    }
    None
}

fn tokenize_json_value_tail(val_str: &str) -> Vec<(JsonTokenKind, String)> {
    let (core, trailing_comma) = match val_str.strip_suffix(',') {
        Some(c) => (c, true),
        None => (val_str, false),
    };

    let kind = if core.starts_with('"') {
        JsonTokenKind::StringVal
    } else if matches!(core, "true" | "false" | "null") {
        JsonTokenKind::BoolOrNull
    } else if core
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit() || c == '-')
    {
        JsonTokenKind::NumberVal
    } else {
        JsonTokenKind::Punctuation
    };

    let mut out = vec![(kind, core.to_string())];
    if trailing_comma {
        out.push((JsonTokenKind::Punctuation, ",".to_string()));
    }
    out
}

pub fn current_hms_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    format!("{h:02}:{m:02}:{s:02}")
}

fn format_micros_to_hms(micros: u64) -> String {
    if micros == 0 {
        return current_hms_string();
    }
    let total_millis = micros / 1000;
    let ms = total_millis % 1000;
    let secs = total_millis / 1000;
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_profile_entry_and_generates_curl() {
        let sample = json!({
            "id": "req-42",
            "method": "POST",
            "uri": "https://api.example.com/v1/login?client=zed",
            "startTime": 1_700_000_000_000_000u64,
            "endTime": 1_700_000_000_145_000u64,
            "requestData": {
                "contentLength": 27,
                "headers": {
                    "content-type": ["application/json"],
                    "authorization": ["Bearer tok_123"]
                }
            },
            "responseData": {
                "statusCode": 200,
                "reasonPhrase": "OK",
                "contentLength": 64,
                "headers": {
                    "content-type": ["application/json; charset=utf-8"]
                }
            },
            "requestBody": "{\"username\":\"alice\"}",
            "responseBody": "{\"ok\":true,\"token\":\"xyz\"}"
        });

        let rec = HttpRequestRecord::parse_from_profile_entry(&sample).expect("parse profile");
        assert_eq!(rec.id, "req-42");
        assert_eq!(rec.method, "POST");
        assert_eq!(rec.host, "api.example.com");
        assert_eq!(rec.path_and_query, "/v1/login?client=zed");
        assert_eq!(rec.status_code, Some(200));
        assert_eq!(rec.duration_ms, Some(145));
        assert_eq!(rec.mime_short, "json");
        assert!(rec.is_json);

        let curl = rec.to_curl_command();
        assert!(curl.contains("curl -X POST 'https://api.example.com/v1/login?client=zed'"));
        assert!(curl.contains("-H 'authorization: Bearer tok_123'"));
        assert!(curl.contains("--data-raw '{\"username\":\"alice\"}'"));
    }

    #[test]
    fn parses_flutter_frame_and_detects_jank() {
        let frame_json = json!({
            "number": 108,
            "build": 4200,
            "raster": 18500,
            "elapsed": 23100,
            "vsyncOverhead": 350
        });

        let frame = FrameTiming::parse_flutter_frame(&frame_json).expect("parse frame");
        assert_eq!(frame.number, 108);
        assert!((frame.build_ms() - 4.2).abs() < 0.01);
        assert!((frame.raster_ms() - 18.5).abs() < 0.01);
        assert!(frame.is_jank(16.6));
        assert!(frame.is_jank(8.3));
    }

    #[test]
    fn parses_widget_summary_tree_and_properties() {
        let tree_json = json!({
            "result": {
                "description": "MyHomePage",
                "widgetRuntimeType": "MyHomePage",
                "valueId": "inspector-1",
                "createdByLocalProject": true,
                "hasChildren": true,
                "creationLocation": {
                    "file": "file:///Users/dev/app/lib/main.dart",
                    "line": 28,
                    "column": 12
                },
                "properties": [
                    {
                        "name": "color",
                        "description": "Color(0xff42a5f5)",
                        "level": "info"
                    }
                ],
                "children": [
                    {
                        "description": "Scaffold",
                        "widgetRuntimeType": "Scaffold",
                        "valueId": "inspector-2",
                        "createdByLocalProject": false,
                        "hasChildren": false,
                        "children": []
                    }
                ]
            }
        });

        let root = WidgetTreeNode::parse_from_json(&tree_json).expect("parse widget tree");
        assert_eq!(root.widget_runtime_type, "MyHomePage");
        assert_eq!(root.value_id, "inspector-1");
        assert!(root.created_by_local_project);
        let loc = root.location.expect("location");
        assert_eq!(loc.line, 28);
        assert_eq!(loc.column, 12);
        assert_eq!(loc.short_label(), "main.dart:28");
        assert_eq!(root.properties.len(), 1);
        assert_eq!(root.properties[0].color_argb, Some(0xff42a5f5));
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].widget_runtime_type, "Scaffold");
    }

    #[test]
    fn parses_logging_event_and_extracts_source_locations() {
        let ev = json!({
            "logRecord": {
                "level": 1000,
                "loggerName": { "valueAsString": "bloc" },
                "message": {
                    "valueAsString": "Unhandled exception in CounterBloc (package:my_app/bloc/counter_bloc.dart:45:9)"
                },
                "stackTrace": {
                    "valueAsString": "#0 CounterBloc.onIncrement (package:my_app/bloc/counter_bloc.dart:45:9)\n#1 main (file:///Users/dev/my_app/lib/main.dart:14:3)"
                }
            }
        });

        let entry = LogEntry::parse_logging_stream_event(&ev, 7).expect("parse log");
        assert_eq!(entry.seq, 7);
        assert_eq!(entry.kind, LogKind::FlutterError);
        assert_eq!(entry.logger_name, "bloc");
        assert_eq!(entry.source_refs.len(), 2);
        assert_eq!(
            entry.source_refs[0],
            SourceLocation {
                uri: "package:my_app/bloc/counter_bloc.dart".into(),
                line: 45,
                column: 9
            }
        );
        assert_eq!(
            entry.source_refs[1],
            SourceLocation {
                uri: "file:///Users/dev/my_app/lib/main.dart".into(),
                line: 14,
                column: 3
            }
        );
    }
}
