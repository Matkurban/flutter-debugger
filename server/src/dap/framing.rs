use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::cache::DiskCache;

#[derive(Debug, Default)]
pub struct ProxySessionState {
    pub project_root: Option<PathBuf>,
    pub mode: String,
    pub vm_service_uri: Option<String>,
    pub devtools_uri: Option<String>,
    pub injected_seqs: HashSet<i64>,
    pub next_injected_seq: i64,
}

impl ProxySessionState {
    pub fn new() -> Self {
        Self {
            project_root: None,
            mode: "debug".to_string(),
            vm_service_uri: None,
            devtools_uri: None,
            injected_seqs: HashSet::new(),
            next_injected_seq: 900_000,
        }
    }

    pub fn allocate_injected_seq(&mut self) -> i64 {
        let seq = self.next_injected_seq;
        self.next_injected_seq += 1;
        self.injected_seqs.insert(seq);
        seq
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

/// Enhances a `launch` or `attach` DAP request with persisted `deviceId`, `flavor`,
/// and `dartDefineFromFile` normalization, and returns `(cwd, flutter_mode)`.
pub fn enrich_launch_or_attach(
    msg: &mut Value,
    cache: &DiskCache,
    default_cwd: &Path,
) -> Option<(PathBuf, String)> {
    let command = msg.get("command").and_then(Value::as_str)?;
    if command != "launch" && command != "attach" {
        return None;
    }

    let args = msg.get_mut("arguments")?.as_object_mut()?;

    let cwd = args
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| default_cwd.to_path_buf());

    args.entry("cwd".to_string())
        .or_insert_with(|| json!(cwd.to_string_lossy()));

    let mode = args
        .get("flutterMode")
        .and_then(Value::as_str)
        .unwrap_or("debug")
        .to_ascii_lowercase();

    let has_device = args
        .get("deviceId")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty());

    if !has_device && let Some(selected) = cache.get_selected_device(&cwd) {
        args.insert("deviceId".to_string(), json!(selected));
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

    Some((cwd, mode))
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
    fn enriches_launch_with_selected_device_and_flavor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::with_root(dir.path().to_path_buf());
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

        let (cwd, mode) = enrich_launch_or_attach(&mut req, &cache, dir.path()).expect("enriched");
        assert_eq!(cwd, dir.path());
        assert_eq!(mode, "debug");
        assert_eq!(req["arguments"]["deviceId"], "emulator-5554");
        assert_eq!(
            req["arguments"]["toolArgs"],
            json!(["--flavor", "staging", "--dart-define-from-file=env/staging.json"])
        );
    }

    #[test]
    fn captures_debugger_uris_event() {
        let mut state = ProxySessionState::new();
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
