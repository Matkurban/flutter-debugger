use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::oneshot;

use crate::cache::{
    DiskCache, extract_device_flag_from_args, now_unix, resolve_effective_device,
};
use crate::ipc::{DebugLibraryScope, ExceptionPauseMode, OverlayState};
use crate::project::{ProjectKind, find_project_for_path, resolve_sdk_executable};

/// Resolved target info computed lazily when `launch` or `attach` arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAdapterTarget {
    pub project_root: PathBuf,
    pub project_kind: ProjectKind,
    pub executable: String,
    pub is_test: bool,
    pub mode: String,
    pub device_id: Option<String>,
    pub device_notice: Option<String>,
}

pub struct ProxySessionState {
    pub session_id: String,
    pub started_at: u64,
    pub project_root: Option<PathBuf>,
    pub mode: String,
    pub device_id: Option<String>,
    pub is_test: bool,
    pub terminated: bool,
    pub vm_service_uri: Option<String>,
    pub devtools_uri: Option<String>,
    pub overlays: OverlayState,
    pub exception_pause_mode: ExceptionPauseMode,
    pub debug_library_scope: DebugLibraryScope,
    pub service_extension_isolates: HashMap<String, String>,
    pub main_isolate_id: Option<String>,
    pub suppressed_seqs: HashSet<i64>,
    pub pending_injected: HashMap<i64, oneshot::Sender<Value>>,
    pub next_injected_seq: i64,
    pub next_proxy_seq: i64,
}

impl ProxySessionState {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            started_at: now_unix(),
            project_root: None,
            mode: "debug".to_string(),
            device_id: None,
            is_test: false,
            terminated: false,
            vm_service_uri: None,
            devtools_uri: None,
            overlays: OverlayState::default(),
            exception_pause_mode: ExceptionPauseMode::None,
            debug_library_scope: DebugLibraryScope::MyCodeOnly,
            service_extension_isolates: HashMap::new(),
            main_isolate_id: None,
            suppressed_seqs: HashSet::new(),
            pending_injected: HashMap::new(),
            next_injected_seq: 900_000,
            next_proxy_seq: 800_000,
        }
    }

    pub fn sync_from_zed_request(&mut self, msg: &Value) {
        if msg.get("type").and_then(Value::as_str) != Some("request") {
            return;
        }
        let cmd = msg.get("command").and_then(Value::as_str).unwrap_or_default();
        let Some(args) = msg.get("arguments").and_then(Value::as_object) else {
            return;
        };

        match cmd {
            "launch" | "attach" => {
                let debug_sdk = args
                    .get("debugSdkLibraries")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let debug_pkg = args
                    .get("debugExternalPackageLibraries")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.debug_library_scope =
                    DebugLibraryScope::from_dap_flags(debug_sdk, debug_pkg);
            }
            "setExceptionBreakpoints" => {
                let filters: Vec<String> = args
                    .get("filters")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .map(ToOwned::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                self.exception_pause_mode = ExceptionPauseMode::from_dap_filters(&filters);
            }
            _ => {}
        }
    }

    pub fn allocate_injected_request(&mut self) -> (i64, oneshot::Receiver<Value>) {
        let seq = self.next_injected_seq;
        self.next_injected_seq += 1;
        let (tx, rx) = oneshot::channel();
        self.pending_injected.insert(seq, tx);
        (seq, rx)
    }

    pub fn allocate_proxy_seq(&mut self) -> i64 {
        let seq = self.next_proxy_seq;
        self.next_proxy_seq += 1;
        seq
    }

    pub fn register_suppressed_waiter(&mut self, seq: i64) -> oneshot::Receiver<Value> {
        let (tx, rx) = oneshot::channel();
        self.pending_injected.insert(seq, tx);
        rx
    }
}

/// Reads a single Content-Length framed DAP JSON message.
pub async fn read_dap_message<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    let mut header_line = String::new();

    loop {
        header_line.clear();
        let n = reader.read_line(&mut header_line).await?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = header_line.trim();
        if trimmed.is_empty() {
            if content_length.is_some() {
                break;
            }
            continue;
        }
        if let Some((name, val)) = trimmed.split_once(':')
            && name.trim().eq_ignore_ascii_case("Content-Length")
            && let Ok(len) = val.trim().parse::<usize>()
        {
            content_length = Some(len);
        }
    }

    let len = content_length.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Missing Content-Length header in DAP message",
        )
    })?;

    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await?;

    let value: Value = serde_json::from_slice(&body)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    Ok(Some(value))
}

/// Writes a single Content-Length framed DAP JSON message.
pub async fn write_dap_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Value,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(message)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    let header = format!("Content-Length: {}\r\n\r\n", bytes.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await
}

/// Builds an immediate synthetic DAP `initialize` response with a non-colliding `seq`
/// so Zed proceeds to send `launch`/`attach`.
pub fn build_synthetic_initialize_response(proxy_seq: i64, request_seq: i64) -> Value {
    json!({
        "seq": proxy_seq,
        "type": "response",
        "request_seq": request_seq,
        "success": true,
        "command": "initialize",
        "body": {
            "supportsConfigurationDoneRequest": true,
            "supportsEvaluateForHovers": true,
            "supportsConditionalBreakpoints": true,
            "supportsHitConditionalBreakpoints": true,
            "supportsLogPoints": true,
            "supportsFunctionBreakpoints": false,
            "supportsExceptionInfoRequest": true,
            "supportsTerminateRequest": true,
            "supportsClipboardContext": true,
            "supportsDelayedStackTraceLoading": true,
            "exceptionBreakpointFilters": [
                { "filter": "All", "label": "All Exceptions", "default": false },
                { "filter": "Unhandled", "label": "Uncaught Exceptions", "default": false }
            ]
        }
    })
}

/// Builds a clean DAP response (success or failure) emitted directly by the proxy.
pub fn build_proxy_response(
    proxy_seq: i64,
    request_seq: i64,
    command: &str,
    success: bool,
    message: Option<&str>,
) -> Value {
    let mut obj = json!({
        "seq": proxy_seq,
        "type": "response",
        "request_seq": request_seq,
        "success": success,
        "command": command
    });
    if let Some(msg) = message {
        obj["message"] = json!(msg);
    }
    obj
}

/// Determines whether a `launch` request targets a Dart/Flutter test file or folder.
pub fn is_test_launch_request(args: &serde_json::Map<String, Value>) -> bool {
    if args.get("isTest").and_then(Value::as_bool) == Some(true) {
        return true;
    }

    if let Some(program) = args.get("program").and_then(Value::as_str) {
        let normalized = program.replace('\\', "/");
        if normalized == "test"
            || normalized == "integration_test"
            || normalized.starts_with("test/")
            || normalized.starts_with("integration_test/")
            || normalized.starts_with("test_driver/")
            || normalized.contains("/test/")
            || normalized.contains("/integration_test/")
            || normalized.ends_with("_test.dart")
        {
            return true;
        }
    }

    false
}

/// Enhances a `launch` or `attach` DAP request with:
/// - Monorepo-aware `cwd` resolution (walks up from `program` first)
/// - FVM/SDK executable detection & `--test` detection
/// - Device resolution translated into `toolArgs: ["-d", "<deviceId>"]` (since `flutter debug_adapter`
///   ignores the top-level `deviceId` DAP field and only reads `-d` / `--device-id` in `toolArgs`)
/// - `flutterMode` -> `--profile` / `--release` (`+ noDebug: true`) in `toolArgs`
/// - `flavor` -> `--flavor` and `dartDefineFromFile` -> `--dart-define-from-file=...`
pub fn resolve_and_enrich_launch_or_attach(
    msg: &mut Value,
    cache: &DiskCache,
    default_cwd: &Path,
    custom_sdk_path: Option<&str>,
) -> Option<ResolvedAdapterTarget> {
    let command = msg.get("command").and_then(Value::as_str)?;
    if command != "launch" && command != "attach" {
        return None;
    }
    let is_launch = command == "launch";

    let args = msg.get_mut("arguments")?.as_object_mut()?;

    let raw_cwd = args
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| default_cwd.to_path_buf());

    let program_str = args
        .get("program")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let candidate_path = match &program_str {
        Some(prog) => {
            let p = Path::new(prog);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                raw_cwd.join(p)
            }
        }
        None => raw_cwd.clone(),
    };

    let project_info =
        find_project_for_path(&candidate_path).or_else(|| find_project_for_path(&raw_cwd));

    let project_root = project_info
        .as_ref()
        .map(|p| p.root.clone())
        .unwrap_or_else(|| raw_cwd.clone());
    let project_kind = project_info
        .as_ref()
        .map(|p| p.kind)
        .unwrap_or(ProjectKind::Flutter);

    args.insert("cwd".to_string(), json!(project_root.to_string_lossy()));

    let is_test = is_launch && is_test_launch_request(args);

    let mode = args
        .get("flutterMode")
        .and_then(Value::as_str)
        .unwrap_or("debug")
        .to_ascii_lowercase();

    let is_integration_test = program_str
        .as_deref()
        .is_some_and(|p| p.replace('\\', "/").contains("integration_test"));
    let needs_device = project_kind == ProjectKind::Flutter && (!is_test || is_integration_test);

    let mut tool_args: Vec<String> = args
        .get("toolArgs")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();

    let prog_args: Vec<String> = args
        .get("args")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();

    let explicit_device_id = args
        .get("deviceId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let existing_cli_device = extract_device_flag_from_args(&tool_args)
        .or_else(|| extract_device_flag_from_args(&prog_args));

    let (device_id, device_notice, inject_device_flag) =
        if let Some(cli_dev) = existing_cli_device {
            (Some(cli_dev), None, false)
        } else if needs_device {
            match resolve_effective_device(cache, &project_root, explicit_device_id.as_deref()) {
                Some(resolved_dev) => {
                    (Some(resolved_dev.id), resolved_dev.auto_selected_notice, true)
                }
                None => (None, None, false),
            }
        } else {
            (explicit_device_id, None, false)
        };

    let flavor = args
        .get("flavor")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let dart_define_file = args
        .get("dartDefineFromFile")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let need_profile_flag =
        is_launch && project_kind == ProjectKind::Flutter && !is_test && mode == "profile";
    let need_release_flag =
        is_launch && project_kind == ProjectKind::Flutter && !is_test && mode == "release";

    if need_release_flag && !args.contains_key("noDebug") {
        args.insert("noDebug".to_string(), json!(true));
    }

    // Default to VS Code Flutter "Debug my code" mode when not explicitly configured
    if !args.contains_key("debugSdkLibraries") {
        args.insert("debugSdkLibraries".to_string(), json!(false));
    }
    if !args.contains_key("debugExternalPackageLibraries") {
        args.insert("debugExternalPackageLibraries".to_string(), json!(false));
    }

    if inject_device_flag
        || flavor.is_some()
        || dart_define_file.is_some()
        || need_profile_flag
        || need_release_flag
    {
        if inject_device_flag && let Some(ref dev) = device_id {
            tool_args.push("-d".to_string());
            tool_args.push(dev.clone());
        }

        if need_profile_flag && !tool_args.iter().any(|a| a == "--profile") {
            tool_args.push("--profile".to_string());
        }
        if need_release_flag && !tool_args.iter().any(|a| a == "--release") {
            tool_args.push("--release".to_string());
        }

        if let Some(flav) = flavor
            && !tool_args.iter().any(|a| a == "--flavor")
        {
            tool_args.push("--flavor".to_string());
            tool_args.push(flav);
        }

        if let Some(define_file) = dart_define_file
            && !tool_args
                .iter()
                .any(|a| a.starts_with("--dart-define-from-file"))
        {
            tool_args.push(format!("--dart-define-from-file={define_file}"));
        }

        args.insert("toolArgs".to_string(), json!(tool_args));
    }

    let executable = resolve_sdk_executable(&project_root, project_kind, custom_sdk_path);

    Some(ResolvedAdapterTarget {
        project_root,
        project_kind,
        executable,
        is_test,
        mode,
        device_id,
        device_notice,
    })
}

/// Inspects outgoing adapter events to capture VM Service / DevTools URIs,
/// `flutter.appStart` metadata, and `dart.serviceExtensionAdded` isolate mappings.
pub fn inspect_adapter_event(msg: &Value, state: &mut ProxySessionState) -> bool {
    if msg.get("type").and_then(Value::as_str) != Some("event") {
        return false;
    }

    let event = msg.get("event").and_then(Value::as_str).unwrap_or_default();
    let Some(body) = msg.get("body") else {
        return false;
    };

    let mut changed = false;

    if event == "dart.debuggerUris" {
        if let Some(vm_uri) = body.get("vmServiceUri").and_then(Value::as_str)
            && state.vm_service_uri.as_deref() != Some(vm_uri)
        {
            state.vm_service_uri = Some(vm_uri.to_string());
            changed = true;
        }
        if let Some(dt_uri) = body.get("devtoolsUri").and_then(Value::as_str)
            && state.devtools_uri.as_deref() != Some(dt_uri)
        {
            state.devtools_uri = Some(dt_uri.to_string());
            changed = true;
        }
    } else if event == "flutter.appStart" {
        if let Some(dev_id) = body.get("deviceId").and_then(Value::as_str)
            && !dev_id.trim().is_empty()
            && state.device_id.as_deref() != Some(dev_id)
        {
            state.device_id = Some(dev_id.to_string());
            changed = true;
        }
        if let Some(mode) = body.get("mode").and_then(Value::as_str)
            && !mode.trim().is_empty()
            && state.mode != mode
        {
            state.mode = mode.to_string();
            changed = true;
        }
    } else if event == "dart.serviceExtensionAdded" {
        if let (Some(ext_rpc), Some(isolate_id)) = (
            body.get("extensionRPC").and_then(Value::as_str),
            body.get("isolateId").and_then(Value::as_str),
        ) {
            state
                .service_extension_isolates
                .insert(ext_rpc.to_string(), isolate_id.to_string());
            state.main_isolate_id = Some(isolate_id.to_string());
        }
    } else if event == "output"
        && let Some(output) = body.get("output").and_then(Value::as_str)
        && state.devtools_uri.is_none()
        && let Some(uri) = extract_local_devtools_url(output)
    {
        state.devtools_uri = Some(uri);
        changed = true;
    }

    changed
}

/// Determines whether an incoming message from `flutter`/`dart debug_adapter` indicates
/// that the application or debug session has terminated, exited, stopped, or failed to launch.
pub fn is_adapter_termination_event(msg: &Value) -> bool {
    match msg.get("type").and_then(Value::as_str) {
        Some("event") => {
            let event = msg.get("event").and_then(Value::as_str).unwrap_or_default();
            if matches!(event, "terminated" | "exited" | "flutter.appStop") {
                return true;
            }
            if event == "flutter.forwardedEvent" {
                let inner = msg
                    .get("body")
                    .and_then(|b| b.get("event"))
                    .and_then(Value::as_str);
                return inner == Some("app.stop");
            }
            false
        }
        Some("response") => {
            let cmd = msg.get("command").and_then(Value::as_str).unwrap_or_default();
            let success = msg.get("success").and_then(Value::as_bool).unwrap_or(true);
            matches!(cmd, "launch" | "attach") && !success
        }
        _ => false,
    }
}

fn extract_local_devtools_url(text: &str) -> Option<String> {
    for token in text.split_whitespace() {
        let cleaned = token.trim_matches(|c: char| matches!(c, '(' | ')' | ',' | '.' | '"' | '\''));
        if let Ok(parsed) = url::Url::parse(cleaned)
            && matches!(parsed.scheme(), "http" | "https")
            && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost"))
            && (cleaned.contains("devtools") || parsed.query().is_some())
        {
            return Some(cleaned.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::FlutterDevice;
    use std::fs;

    fn make_flutter_project(dir: &Path, platforms: &[&str]) {
        fs::write(
            dir.join("pubspec.yaml"),
            "name: demo\nflutter:\n  uses-material-design: true\n",
        )
        .expect("write pubspec");
        for p in platforms {
            fs::create_dir_all(dir.join(p)).expect("mkdir platform");
        }
    }

    #[test]
    fn maps_flutter_mode_profile_and_release_into_tool_args_and_no_debug() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::with_root(dir.path().join(".cache"));
        make_flutter_project(dir.path(), &[]);

        let mut profile_req = json!({
            "seq": 2,
            "type": "request",
            "command": "launch",
            "arguments": {
                "program": "lib/main.dart",
                "flutterMode": "profile"
            }
        });
        let resolved_p =
            resolve_and_enrich_launch_or_attach(&mut profile_req, &cache, dir.path(), None)
                .expect("profile");
        assert_eq!(resolved_p.mode, "profile");
        assert_eq!(profile_req["arguments"]["toolArgs"], json!(["--profile"]));
        assert!(profile_req["arguments"].get("noDebug").is_none());

        let mut release_req = json!({
            "seq": 3,
            "type": "request",
            "command": "launch",
            "arguments": {
                "program": "lib/main.dart",
                "flutterMode": "release"
            }
        });
        let resolved_r =
            resolve_and_enrich_launch_or_attach(&mut release_req, &cache, dir.path(), None)
                .expect("release");
        assert_eq!(resolved_r.mode, "release");
        assert_eq!(release_req["arguments"]["toolArgs"], json!(["--release"]));
        assert_eq!(release_req["arguments"]["noDebug"], json!(true));
    }

    #[test]
    fn translates_explicit_device_id_into_tool_args_flag() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::with_root(dir.path().join(".cache"));
        make_flutter_project(dir.path(), &["macos"]);

        let mut req = json!({
            "seq": 2,
            "type": "request",
            "command": "launch",
            "arguments": {
                "program": "lib/main.dart",
                "deviceId": "macos"
            }
        });

        let resolved =
            resolve_and_enrich_launch_or_attach(&mut req, &cache, dir.path(), None).expect("ok");
        assert_eq!(resolved.device_id.as_deref(), Some("macos"));
        assert_eq!(resolved.device_notice, None);
        assert_eq!(req["arguments"]["toolArgs"], json!(["-d", "macos"]));
    }

    #[test]
    fn injects_selected_device_into_tool_args_for_launch_and_attach() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::with_root(dir.path().join(".cache"));
        make_flutter_project(dir.path(), &["macos"]);
        cache.set_selected_device(dir.path(), "macos");

        for cmd in ["launch", "attach"] {
            let mut req = json!({
                "seq": 2,
                "type": "request",
                "command": cmd,
                "arguments": {
                    "program": "lib/main.dart"
                }
            });
            let resolved =
                resolve_and_enrich_launch_or_attach(&mut req, &cache, dir.path(), None)
                    .expect("resolved");
            assert_eq!(resolved.device_id.as_deref(), Some("macos"));
            assert_eq!(resolved.device_notice, None);
            assert_eq!(req["arguments"]["toolArgs"], json!(["-d", "macos"]));
        }
    }

    #[test]
    fn does_not_duplicate_device_flag_when_tool_args_already_specifies_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::with_root(dir.path().join(".cache"));
        make_flutter_project(dir.path(), &["macos", "web"]);
        cache.set_selected_device(dir.path(), "macos");

        let mut req = json!({
            "seq": 2,
            "type": "request",
            "command": "launch",
            "arguments": {
                "program": "lib/main.dart",
                "toolArgs": ["-d", "chrome"]
            }
        });

        let resolved =
            resolve_and_enrich_launch_or_attach(&mut req, &cache, dir.path(), None).expect("ok");
        assert_eq!(resolved.device_id.as_deref(), Some("chrome"));
        assert_eq!(resolved.device_notice, None);
        assert_eq!(req["arguments"]["toolArgs"], json!(["-d", "chrome"]));
    }

    #[test]
    fn auto_selects_platform_compatible_device_by_priority_and_respects_enabled_platforms() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::with_root(dir.path().join(".cache"));

        let devices = vec![
            FlutterDevice {
                id: "chrome".into(),
                name: "Chrome".into(),
                target_platform: "web-javascript".into(),
                emulator: false,
                sdk: "Google Chrome".into(),
            },
            FlutterDevice {
                id: "iphone-wifi-1".into(),
                name: "iPhone (wireless)".into(),
                target_platform: "ios".into(),
                emulator: false,
                sdk: "iOS 18".into(),
            },
            FlutterDevice {
                id: "macos".into(),
                name: "macOS".into(),
                target_platform: "darwin".into(),
                emulator: false,
                sdk: "macOS 15".into(),
            },
        ];
        cache.save_devices(&devices);

        // Project with `macos/` enabled -> Tier 2 (macos) beats Tier 3 (wireless) & Tier 4 (chrome)
        let desktop_proj = dir.path().join("desktop_app");
        fs::create_dir_all(&desktop_proj).expect("mkdir");
        make_flutter_project(&desktop_proj, &["macos", "ios", "web"]);

        let mut req = json!({
            "seq": 2,
            "type": "request",
            "command": "launch",
            "arguments": {
                "cwd": desktop_proj.to_string_lossy(),
                "program": "lib/main.dart"
            }
        });
        let resolved =
            resolve_and_enrich_launch_or_attach(&mut req, &cache, &desktop_proj, None).expect("ok");
        assert_eq!(resolved.device_id.as_deref(), Some("macos"));
        assert!(resolved.device_notice.is_some());
        assert_eq!(req["arguments"]["toolArgs"], json!(["-d", "macos"]));

        // Mobile-only project (`ios/` + `android/`, NO `macos/`) must NOT pick `macos`
        let mobile_proj = dir.path().join("mobile_app");
        fs::create_dir_all(&mobile_proj).expect("mkdir");
        make_flutter_project(&mobile_proj, &["ios", "android"]);

        let mut mobile_req = json!({
            "seq": 3,
            "type": "request",
            "command": "launch",
            "arguments": {
                "cwd": mobile_proj.to_string_lossy(),
                "program": "lib/main.dart"
            }
        });
        let mobile_resolved =
            resolve_and_enrich_launch_or_attach(&mut mobile_req, &cache, &mobile_proj, None)
                .expect("ok");
        assert_eq!(mobile_resolved.device_id.as_deref(), Some("iphone-wifi-1"));
        assert!(mobile_resolved.device_notice.is_some());
        assert_eq!(
            mobile_req["arguments"]["toolArgs"],
            json!(["-d", "iphone-wifi-1"])
        );
    }

    #[test]
    fn inspects_app_start_and_service_extension_added_events() {
        let mut state = ProxySessionState::new("sess-1".into());

        let app_start = json!({
            "type": "event",
            "event": "flutter.appStart",
            "body": {
                "deviceId": "macos",
                "mode": "profile"
            }
        });
        assert!(inspect_adapter_event(&app_start, &mut state));
        assert_eq!(state.device_id.as_deref(), Some("macos"));
        assert_eq!(state.mode, "profile");

        let ext_added = json!({
            "type": "event",
            "event": "dart.serviceExtensionAdded",
            "body": {
                "extensionRPC": "ext.flutter.inspector.show",
                "isolateId": "isolates/12345"
            }
        });
        let _ = inspect_adapter_event(&ext_added, &mut state);
        assert_eq!(
            state
                .service_extension_isolates
                .get("ext.flutter.inspector.show")
                .map(String::as_str),
            Some("isolates/12345")
        );
        assert_eq!(state.main_isolate_id.as_deref(), Some("isolates/12345"));
    }

    #[test]
    fn resolves_monorepo_subpackage_and_test_flag() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::with_root(dir.path().join(".cache"));

        let subpkg = dir.path().join("packages").join("core_dart");
        fs::create_dir_all(subpkg.join("test")).expect("mkdir test");
        fs::write(
            subpkg.join("pubspec.yaml"),
            "name: core_dart\nenvironment:\n  sdk: ^3.7.0\n",
        )
        .expect("write pubspec");
        let test_file = subpkg.join("test").join("math_test.dart");
        fs::write(&test_file, "void main() {}").expect("write test");

        let mut req = json!({
            "seq": 2,
            "type": "request",
            "command": "launch",
            "arguments": {
                "cwd": dir.path().to_string_lossy(),
                "program": "packages/core_dart/test/math_test.dart"
            }
        });

        let resolved = resolve_and_enrich_launch_or_attach(&mut req, &cache, dir.path(), None)
            .expect("resolved");
        assert_eq!(resolved.project_root, subpkg);
        assert_eq!(resolved.project_kind, ProjectKind::Dart);
        assert!(resolved.is_test);
    }

    #[test]
    fn defaults_exception_breakpoints_off_and_injects_just_my_code_flags() {
        let init = build_synthetic_initialize_response(800_000, 1);
        let filters = init["body"]["exceptionBreakpointFilters"]
            .as_array()
            .expect("filters array");
        assert!(
            filters
                .iter()
                .all(|f| f.get("default").and_then(Value::as_bool) == Some(false)),
            "Both All and Unhandled exception breakpoint filters must default to false"
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::with_root(dir.path().join(".cache"));
        make_flutter_project(dir.path(), &["macos"]);

        let mut req = json!({
            "seq": 2,
            "type": "request",
            "command": "launch",
            "arguments": {
                "program": "lib/main.dart"
            }
        });
        let _ = resolve_and_enrich_launch_or_attach(&mut req, &cache, dir.path(), None)
            .expect("launch");
        assert_eq!(req["arguments"]["debugSdkLibraries"], json!(false));
        assert_eq!(
            req["arguments"]["debugExternalPackageLibraries"],
            json!(false)
        );

        // User explicit true must be respected
        let mut custom_req = json!({
            "seq": 3,
            "type": "request",
            "command": "launch",
            "arguments": {
                "program": "lib/main.dart",
                "debugSdkLibraries": true,
                "debugExternalPackageLibraries": true
            }
        });
        let _ = resolve_and_enrich_launch_or_attach(&mut custom_req, &cache, dir.path(), None)
            .expect("custom launch");
        assert_eq!(custom_req["arguments"]["debugSdkLibraries"], json!(true));
        assert_eq!(
            custom_req["arguments"]["debugExternalPackageLibraries"],
            json!(true)
        );
    }

    #[test]
    fn detects_adapter_termination_events_and_failed_launches() {
        assert!(is_adapter_termination_event(&json!({
            "type": "event",
            "event": "terminated"
        })));
        assert!(is_adapter_termination_event(&json!({
            "type": "event",
            "event": "exited",
            "body": { "exitCode": 0 }
        })));
        assert!(is_adapter_termination_event(&json!({
            "type": "event",
            "event": "flutter.appStop",
            "body": { "appId": "app-1" }
        })));
        assert!(is_adapter_termination_event(&json!({
            "type": "event",
            "event": "flutter.forwardedEvent",
            "body": { "event": "app.stop", "params": { "appId": "app-1" } }
        })));
        assert!(is_adapter_termination_event(&json!({
            "type": "response",
            "command": "launch",
            "success": false,
            "message": "Build failed"
        })));
        assert!(is_adapter_termination_event(&json!({
            "type": "response",
            "command": "attach",
            "success": false,
            "message": "VM Service unreachable"
        })));

        // Non-terminating events (especially breakpoint `stopped` and `output`) must return false!
        assert!(!is_adapter_termination_event(&json!({
            "type": "event",
            "event": "stopped",
            "body": { "reason": "breakpoint", "threadId": 1 }
        })));
        assert!(!is_adapter_termination_event(&json!({
            "type": "event",
            "event": "output",
            "body": { "output": "Lost connection to device.\n" }
        })));
        assert!(!is_adapter_termination_event(&json!({
            "type": "event",
            "event": "flutter.forwardedEvent",
            "body": { "event": "app.progress" }
        })));
        assert!(!is_adapter_termination_event(&json!({
            "type": "response",
            "command": "launch",
            "success": true
        })));
    }
}
