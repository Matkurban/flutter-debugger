use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::cache::{DiskCache, normalize_project_key, now_unix};

/// Supported Flutter Service Extension overlays & runtime toggles.
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
    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "inspector" | "widget-inspector" | "widgetinspector" => Some(Self::Inspector),
            "performance" | "performance-overlay" | "performanceoverlay" => Some(Self::Performance),
            "debug-paint" | "debugpaint" | "paint" => Some(Self::DebugPaint),
            "baselines" | "paint-baselines" | "debugpaintbaselines" => Some(Self::Baselines),
            "repaint" | "repaint-rainbow" | "repaintrainbow" => Some(Self::RepaintRainbow),
            "slow-anim" | "slow-animations" | "slowanimations" | "slow" => {
                Some(Self::SlowAnimations)
            }
            "banner" | "debug-banner" | "debugbanner" => Some(Self::DebugBanner),
            "oversized" | "oversized-images" | "invert-oversized-images" => {
                Some(Self::OversizedImages)
            }
            "platform" | "platform-override" | "platformoverride" => Some(Self::PlatformOverride),
            "brightness" | "brightness-override" | "brightnessoverride" | "theme" => {
                Some(Self::BrightnessOverride)
            }
            _ => None,
        }
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
            Self::OversizedImages => "Invert Oversized Images",
            Self::PlatformOverride => "Target Platform",
            Self::BrightnessOverride => "Brightness",
        }
    }
}

/// Tracks toggle state for Flutter Service Extensions within an active session.
#[derive(Debug, Clone)]
pub struct OverlayState {
    pub inspector: bool,
    pub performance: bool,
    pub debug_paint: bool,
    pub baselines: bool,
    pub repaint_rainbow: bool,
    pub slow_animations: bool,
    pub debug_banner: bool,
    pub oversized_images: bool,
    pub platform_index: usize,
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
            platform_index: 0,
            dark_brightness: false,
        }
    }
}

impl OverlayState {
    /// Flips the requested overlay and returns `(service_extension_method, params, status_label)`.
    pub fn toggle(&mut self, overlay: ServiceOverlay) -> (&'static str, Value, String) {
        match overlay {
            ServiceOverlay::Inspector => {
                self.inspector = !self.inspector;
                let v = self.inspector;
                (
                    "ext.flutter.inspector.show",
                    serde_json::json!({ "enabled": v.to_string() }),
                    format!("{}: {}", overlay.label(), if v { "ON" } else { "OFF" }),
                )
            }
            ServiceOverlay::Performance => {
                self.performance = !self.performance;
                let v = self.performance;
                (
                    "ext.flutter.showPerformanceOverlay",
                    serde_json::json!({ "enabled": v.to_string() }),
                    format!("{}: {}", overlay.label(), if v { "ON" } else { "OFF" }),
                )
            }
            ServiceOverlay::DebugPaint => {
                self.debug_paint = !self.debug_paint;
                let v = self.debug_paint;
                (
                    "ext.flutter.debugPaint",
                    serde_json::json!({ "enabled": v.to_string() }),
                    format!("{}: {}", overlay.label(), if v { "ON" } else { "OFF" }),
                )
            }
            ServiceOverlay::Baselines => {
                self.baselines = !self.baselines;
                let v = self.baselines;
                (
                    "ext.flutter.debugPaintBaselinesEnabled",
                    serde_json::json!({ "enabled": v.to_string() }),
                    format!("{}: {}", overlay.label(), if v { "ON" } else { "OFF" }),
                )
            }
            ServiceOverlay::RepaintRainbow => {
                self.repaint_rainbow = !self.repaint_rainbow;
                let v = self.repaint_rainbow;
                (
                    "ext.flutter.repaintRainbow",
                    serde_json::json!({ "enabled": v.to_string() }),
                    format!("{}: {}", overlay.label(), if v { "ON" } else { "OFF" }),
                )
            }
            ServiceOverlay::SlowAnimations => {
                self.slow_animations = !self.slow_animations;
                let v = self.slow_animations;
                (
                    "ext.flutter.timeDilation",
                    serde_json::json!({ "timeDilation": if v { "5.0" } else { "1.0" } }),
                    format!("{}: {}", overlay.label(), if v { "ON" } else { "OFF" }),
                )
            }
            ServiceOverlay::DebugBanner => {
                self.debug_banner = !self.debug_banner;
                let v = self.debug_banner;
                (
                    "ext.flutter.debugAllowBanner",
                    serde_json::json!({ "enabled": v.to_string() }),
                    format!("{}: {}", overlay.label(), if v { "ON" } else { "OFF" }),
                )
            }
            ServiceOverlay::OversizedImages => {
                self.oversized_images = !self.oversized_images;
                let v = self.oversized_images;
                (
                    "ext.flutter.invertOversizedImages",
                    serde_json::json!({ "enabled": v.to_string() }),
                    format!("{}: {}", overlay.label(), if v { "ON" } else { "OFF" }),
                )
            }
            ServiceOverlay::PlatformOverride => {
                const PLATFORMS: [&str; 5] = ["iOS", "android", "macOS", "linux", "windows"];
                let idx = self.platform_index % PLATFORMS.len();
                let platform = PLATFORMS[idx];
                self.platform_index = (idx + 1) % PLATFORMS.len();
                (
                    "ext.flutter.platformOverride",
                    serde_json::json!({ "value": platform }),
                    format!("{}: {platform}", overlay.label()),
                )
            }
            ServiceOverlay::BrightnessOverride => {
                self.dark_brightness = !self.dark_brightness;
                let value = if self.dark_brightness {
                    "Brightness.dark"
                } else {
                    "Brightness.light"
                };
                let display = if self.dark_brightness {
                    "Dark"
                } else {
                    "Light"
                };
                (
                    "ext.flutter.brightnessOverride",
                    serde_json::json!({ "value": value }),
                    format!("{}: {display}", overlay.label()),
                )
            }
        }
    }
}

/// Persistent metadata for a single active Flutter/Dart session (DAP or `ctl run`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveSessionRecord {
    pub session_id: String,
    pub project_root: String,
    #[serde(default)]
    pub device_id: Option<String>,
    pub mode: String,
    pub pid: u32,
    #[serde(alias = "port")]
    pub ipc_port: u16,
    #[serde(default)]
    pub vm_service_uri: Option<String>,
    #[serde(default)]
    pub devtools_uri: Option<String>,
    #[serde(default)]
    pub started_at: u64,
    #[serde(default)]
    pub is_test: bool,
    #[serde(default = "default_session_kind")]
    pub kind: String,
}

fn default_session_kind() -> String {
    "dap".to_string()
}

pub fn generate_session_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("s-{}-{}-{seq}", std::process::id(), now_unix())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase")]
pub enum IpcRequest {
    HotReload,
    HotRestart,
    Stop,
    Status,
    ToggleOverlay {
        overlay: ServiceOverlay,
    },
    CallServiceExtension {
        method: String,
        params: Value,
    },
    OpenDevTools {
        #[serde(default)]
        page: Option<String>,
    },
    Screenshot {
        #[serde(default)]
        output_path: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    pub message: String,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub vm_service_uri: Option<String>,
    #[serde(default)]
    pub devtools_uri: Option<String>,
}

impl IpcResponse {
    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            duration_ms: None,
            mode: None,
            device_id: None,
            vm_service_uri: None,
            devtools_uri: None,
        }
    }
}

pub fn sessions_dir(cache: &DiskCache) -> PathBuf {
    cache.root_dir().join("sessions")
}

pub fn session_file_path_by_id(cache: &DiskCache, session_id: &str) -> PathBuf {
    sessions_dir(cache).join(format!("{session_id}.json"))
}

pub fn write_session_record(
    cache: &DiskCache,
    record: &ActiveSessionRecord,
) -> std::io::Result<PathBuf> {
    let dir = sessions_dir(cache);
    fs::create_dir_all(&dir)?;
    let path = session_file_path_by_id(cache, &record.session_id);
    let data = serde_json::to_vec_pretty(record)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, &path)?;
    Ok(path)
}

pub fn remove_session_record_by_id(cache: &DiskCache, session_id: &str) {
    let path = session_file_path_by_id(cache, session_id);
    let _ = fs::remove_file(path);
}

/// Lists all live active sessions across all projects, automatically pruning dead PIDs
/// and corrupt/legacy session files. Sorted newest-first (`started_at` descending).
pub fn list_active_sessions(cache: &DiskCache) -> Vec<ActiveSessionRecord> {
    let dir = sessions_dir(cache);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut active = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            let _ = fs::remove_file(&path);
            continue;
        };
        let Ok(record) = serde_json::from_slice::<ActiveSessionRecord>(&bytes) else {
            let _ = fs::remove_file(&path);
            continue;
        };
        if !is_pid_alive(record.pid) {
            let _ = fs::remove_file(&path);
            continue;
        }
        active.push(record);
    }

    active.sort_by_key(|a| std::cmp::Reverse(a.started_at));
    active
}

/// Returns all live sessions matching `project_root` (newest first).
pub fn find_sessions_for_project(
    cache: &DiskCache,
    project_root: &Path,
) -> Vec<ActiveSessionRecord> {
    let key = normalize_project_key(project_root);
    list_active_sessions(cache)
        .into_iter()
        .filter(|s| s.project_root == key)
        .collect()
}

/// Returns the newest live session for `project_root` (or globally if `project_root` is `None`).
pub fn read_session_record(cache: &DiskCache, project_root: &Path) -> Option<ActiveSessionRecord> {
    find_sessions_for_project(cache, project_root)
        .into_iter()
        .next()
}

/// Returns the newest live session for `project_root`, falling back to any live session
/// if `project_root` is `None`.
pub fn find_latest_session(
    cache: &DiskCache,
    project_root: Option<&Path>,
) -> Option<ActiveSessionRecord> {
    match project_root {
        Some(root) => read_session_record(cache, root),
        None => list_active_sessions(cache).into_iter().next(),
    }
}

/// Fast OS-level PID liveness check to prevent 800ms TCP connect timeouts on dead sessions.
pub fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        let Ok(pid_i32) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: kill(pid, 0) sends no signal; it only checks process existence.
        let res = unsafe { kill(pid_i32, 0) };
        if res == 0 {
            return true;
        }
        // EPERM (1) means the process exists but belongs to another user; ESRCH (3) means dead.
        matches!(std::io::Error::last_os_error().raw_os_error(), Some(1))
    }

    #[cfg(windows)]
    {
        // On Windows, fall back to assuming alive and letting fast loopback connect verify.
        let _ = pid;
        true
    }
}

/// Sends an IPC command to a specific active session over loopback TCP.
/// Checks PID liveness first and prunes stale session files immediately.
pub async fn send_ipc_to_session(
    cache: &DiskCache,
    record: &ActiveSessionRecord,
    request: &IpcRequest,
) -> Option<IpcResponse> {
    if !is_pid_alive(record.pid) {
        remove_session_record_by_id(cache, &record.session_id);
        return None;
    }

    let addr = format!("127.0.0.1:{}", record.ipc_port);
    let Ok(Ok(mut stream)) =
        tokio::time::timeout(Duration::from_millis(300), TcpStream::connect(&addr)).await
    else {
        remove_session_record_by_id(cache, &record.session_id);
        return None;
    };

    let mut payload = serde_json::to_string(request).ok()?;
    payload.push('\n');
    if stream.write_all(payload.as_bytes()).await.is_err() {
        return None;
    }

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    match tokio::time::timeout(Duration::from_secs(15), reader.read_line(&mut line)).await {
        Ok(Ok(n)) if n > 0 => serde_json::from_str::<IpcResponse>(line.trim()).ok(),
        _ => None,
    }
}

/// Sends an IPC command to the newest live session for `project_root`.
pub async fn send_ipc_request(
    cache: &DiskCache,
    project_root: &Path,
    request: &IpcRequest,
) -> Option<IpcResponse> {
    let sessions = find_sessions_for_project(cache, project_root);
    for record in sessions {
        if let Some(resp) = send_ipc_to_session(cache, &record, request).await {
            return Some(resp);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_session_registry_and_dead_pid_cleanup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::with_root(dir.path().to_path_buf());
        let project_a = dir.path().join("app_a");
        let project_b = dir.path().join("app_b");
        fs::create_dir_all(&project_a).expect("mkdir a");
        fs::create_dir_all(&project_b).expect("mkdir b");

        let alive_pid = std::process::id();
        let dead_pid = 999_999_999u32;

        let rec_a1 = ActiveSessionRecord {
            session_id: "sess-a1".into(),
            project_root: normalize_project_key(&project_a),
            device_id: Some("macos".into()),
            mode: "debug".into(),
            pid: alive_pid,
            ipc_port: 40001,
            vm_service_uri: Some("ws://127.0.0.1:50001/ws".into()),
            devtools_uri: None,
            started_at: 100,
            is_test: false,
            kind: "dap".into(),
        };
        let rec_a2 = ActiveSessionRecord {
            session_id: "sess-a2".into(),
            project_root: normalize_project_key(&project_a),
            device_id: Some("ios".into()),
            mode: "profile".into(),
            pid: alive_pid,
            ipc_port: 40002,
            vm_service_uri: None,
            devtools_uri: None,
            started_at: 200,
            is_test: false,
            kind: "run".into(),
        };
        let rec_dead = ActiveSessionRecord {
            session_id: "sess-dead".into(),
            project_root: normalize_project_key(&project_b),
            device_id: None,
            mode: "debug".into(),
            pid: dead_pid,
            ipc_port: 40003,
            vm_service_uri: None,
            devtools_uri: None,
            started_at: 300,
            is_test: false,
            kind: "dap".into(),
        };

        write_session_record(&cache, &rec_a1).expect("write a1");
        write_session_record(&cache, &rec_a2).expect("write a2");
        write_session_record(&cache, &rec_dead).expect("write dead");

        let all = list_active_sessions(&cache);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].session_id, "sess-a2");
        assert_eq!(all[1].session_id, "sess-a1");
        // Dead session file must have been pruned
        assert!(!session_file_path_by_id(&cache, "sess-dead").exists());

        let latest_a = read_session_record(&cache, &project_a).expect("latest a");
        assert_eq!(latest_a.session_id, "sess-a2");
    }

    #[test]
    fn overlay_state_toggles_all_extensions() {
        let mut state = OverlayState::default();
        let (method, params, label) = state.toggle(ServiceOverlay::Inspector);
        assert_eq!(method, "ext.flutter.inspector.show");
        assert_eq!(params["enabled"], "true");
        assert!(label.contains("ON"));

        let (method, params, label) = state.toggle(ServiceOverlay::BrightnessOverride);
        assert_eq!(method, "ext.flutter.brightnessOverride");
        assert_eq!(params["value"], "Brightness.dark");
        assert!(label.contains("Dark"));

        let (_, params2, label2) = state.toggle(ServiceOverlay::BrightnessOverride);
        assert_eq!(params2["value"], "Brightness.light");
        assert!(label2.contains("Light"));
    }

    #[test]
    fn parses_service_overlay_aliases() {
        assert_eq!(ServiceOverlay::parse("inspector"), Some(ServiceOverlay::Inspector));
        assert_eq!(ServiceOverlay::parse("debug-paint"), Some(ServiceOverlay::DebugPaint));
        assert_eq!(ServiceOverlay::parse("slow-anim"), Some(ServiceOverlay::SlowAnimations));
        assert_eq!(ServiceOverlay::parse("brightness"), Some(ServiceOverlay::BrightnessOverride));
        assert_eq!(ServiceOverlay::parse("oversized"), Some(ServiceOverlay::OversizedImages));
        assert_eq!(ServiceOverlay::parse("unknown"), None);
    }
}
