use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::oneshot;

use crate::cache::DiskCache;
use crate::ipc::OverlayState;
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
}

pub struct ProxySessionState {
    pub session_id: String,
    pub project_root: Option<PathBuf>,
    pub mode: String,
    pub device_id: Option<String>,
    pub is_test: bool,
    pub vm_service_uri: Option<String>,
    pub devtools_uri: Option<String>,
    pub overlays: OverlayState,
    pub suppressed_seqs: HashSet<i64>,
    pub pending_injected: HashMap<i64, oneshot::Sender<Value>>,
    pub next_injected_seq: i64,
}

impl ProxySessionState {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            project_root: None,
            mode: "debug".to_string(),
            device_id: None,
            is_test: false,
            vm_service_uri: None,
            devtools_uri: None,
            overlays: OverlayState::default(),
            suppressed_seqs: HashSet::new(),
            pending_injected: HashMap::new(),
            next_injected_seq: 900_000,
        }
    }

    pub fn allocate_injected_request(&mut self) -> (i64, oneshot::Receiver<Value>) {
        let seq = self.next_injected_seq;
        self.next_injected_seq += 1;
        let (tx, rx) = oneshot::channel();
        self.pending_injected.insert(seq, tx);
        (seq, rx)
    }

    pub fn mark_suppressed(&mut self, seq: i64) {
        self.suppressed_seqs.insert(seq);
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

/// Builds an immediate synthetic DAP `initialize` response so Zed proceeds to send
/// `launch`/`attach` (which carries `cwd`, `program`, and `isTest` for Monorepo/FVM/--test resolution).
pub fn build_synthetic_initialize_response(request_seq: i64) -> Value {
    json!({
        "seq": 1,
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
                { "filter": "Unhandled", "label": "Uncaught Exceptions", "default": true }
            ]
        }
    })
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

/// Enhances a `launch` or `attach` DAP request with Monorepo-aware `cwd` resolution,
/// FVM/SDK detection, `--test` detection, persisted `deviceId`, `flavor`, and `dartDefineFromFile`.
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

    // Resolve the nearest pubspec.yaml starting from `program` (handles Monorepo subpackages!)
    // before falling back to `raw_cwd`.
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

    // Only auto-inject deviceId for Flutter apps or integration_test (not unit/widget tests)
    let is_integration_test = program_str
        .as_deref()
        .is_some_and(|p| p.replace('\\', "/").contains("integration_test"));
    let needs_device = project_kind == ProjectKind::Flutter && (!is_test || is_integration_test);

    let mut device_id = args
        .get("deviceId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    if needs_device
        && device_id.is_none()
        && let Some(selected) = cache.get_selected_device(&project_root)
    {
        args.insert("deviceId".to_string(), json!(selected));
        device_id = Some(selected);
    }

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

    if flavor.is_some() || dart_define_file.is_some() {
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
    })
}

/// Inspects outgoing adapter events to capture VM Service and DevTools URIs.
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
    use std::fs;

    #[tokio::test]
    async fn roundtrips_dap_framing() {
        let msg = json!({"seq": 1, "type": "request", "command": "initialize"});
        let mut buf = Vec::new();
        write_dap_message(&mut buf, &msg).await.expect("write");

        let mut cursor = tokio::io::BufReader::new(buf.as_slice());
        let decoded = read_dap_message(&mut cursor)
            .await
            .expect("read")
            .expect("some");
        assert_eq!(decoded, msg);
    }

    #[test]
    fn resolves_monorepo_subpackage_and_fvm_and_test_flag() {
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
        assert_eq!(resolved.device_id, None);
    }

    #[test]
    fn enriches_flutter_app_launch_with_selected_device_and_flavor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::with_root(dir.path().join(".cache"));
        fs::write(
            dir.path().join("pubspec.yaml"),
            "name: demo\nflutter:\n  uses-material-design: true\n",
        )
        .expect("write pubspec");
        cache.set_selected_device(dir.path(), "emulator-5554");

        let mut req = json!({
            "seq": 2,
            "type": "request",
            "command": "launch",
            "arguments": {
                "program": "lib/main.dart",
                "flavor": "staging",
                "dartDefineFromFile": "env/staging.json"
            }
        });

        let resolved = resolve_and_enrich_launch_or_attach(&mut req, &cache, dir.path(), None)
            .expect("resolved");
        assert_eq!(resolved.project_root, dir.path());
        assert_eq!(resolved.project_kind, ProjectKind::Flutter);
        assert!(!resolved.is_test);
        assert_eq!(resolved.device_id.as_deref(), Some("emulator-5554"));
        assert_eq!(req["arguments"]["deviceId"], "emulator-5554");
        assert_eq!(
            req["arguments"]["toolArgs"],
            json!(["--flavor", "staging", "--dart-define-from-file=env/staging.json"])
        );
    }

    #[test]
    fn detects_explicit_is_test_and_test_paths() {
        let explicit: serde_json::Map<String, Value> =
            serde_json::from_value(json!({ "isTest": true, "program": "lib/foo.dart" })).unwrap();
        assert!(is_test_launch_request(&explicit));

        let unit: serde_json::Map<String, Value> =
            serde_json::from_value(json!({ "program": "test/widget_test.dart" })).unwrap();
        assert!(is_test_launch_request(&unit));

        let app: serde_json::Map<String, Value> =
            serde_json::from_value(json!({ "program": "lib/main.dart" })).unwrap();
        assert!(!is_test_launch_request(&app));
    }

    #[test]
    fn captures_debugger_uris_event() {
        let mut state = ProxySessionState::new("test-sess".into());
        let evt = json!({
            "type": "event",
            "event": "dart.debuggerUris",
            "body": {
                "vmServiceUri": "ws://localhost:50000/vm",
                "devtoolsUri": "http://localhost:9100/devtools"
            }
        });
        assert!(inspect_adapter_event(&evt, &mut state));
        assert_eq!(
            state.devtools_uri.as_deref(),
            Some("http://localhost:9100/devtools")
        );
    }
}
