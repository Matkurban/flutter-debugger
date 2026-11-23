use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;

use dashmap::DashMap;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tower_lsp_server::Client;
use tower_lsp_server::ls_types::{
    Diagnostic, DiagnosticSeverity, MessageType, Position, Range, Uri,
    request::CodeLensRefresh,
};

use crate::lsp::dart_file::DartTestEntry;
use crate::lsp::progress::ProgressHandle;
use crate::project::{ProjectInfo, ProjectKind, resolve_sdk_executable};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestStatus {
    Passed { duration_ms: u64 },
    Failed { duration_ms: u64, error: String },
    Skipped,
}

/// Tracks per-test outcomes parsed from `flutter test --machine` / `dart test --reporter json`.
#[derive(Debug, Clone, Default)]
pub struct TestRunStore {
    /// Keyed by file URI string -> map of `full_name` -> `TestStatus`.
    by_file: Arc<DashMap<String, HashMap<String, TestStatus>>>,
}

impl TestRunStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_file_results(&self, uri: &Uri) -> HashMap<String, TestStatus> {
        self.by_file
            .get(uri.as_str())
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    pub fn update_file_results(&self, uri: &Uri, updates: HashMap<String, TestStatus>, replace: bool) {
        if replace {
            self.by_file.insert(uri.as_str().to_string(), updates);
        } else {
            let mut entry = self.by_file.entry(uri.as_str().to_string()).or_default();
            for (k, v) in updates {
                entry.insert(k, v);
            }
        }
    }
}

/// Incremental state machine for parsing `flutter test --machine` / `dart test --reporter json` NDJSON events.
#[derive(Debug, Default)]
pub struct MachineTestParser {
    id_to_name: HashMap<i64, String>,
    id_to_start_ms: HashMap<i64, u64>,
    id_to_errors: HashMap<i64, Vec<String>>,
    pub outcomes: HashMap<String, TestStatus>,
    pub total_started: usize,
    pub total_done: usize,
    pub failed_count: usize,
}

impl MachineTestParser {
    pub fn feed_line(&mut self, line: &str) {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            return;
        }
        let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
            return;
        };
        let Some(event_type) = val.get("type").and_then(Value::as_str) else {
            return;
        };
        let time_ms = val.get("time").and_then(Value::as_u64).unwrap_or(0);

        match event_type {
            "testStart" => {
                if let Some(test_obj) = val.get("test")
                    && let (Some(id), Some(name)) = (
                        test_obj.get("id").and_then(Value::as_i64),
                        test_obj.get("name").and_then(Value::as_str),
                    )
                {
                    if name.starts_with("loading ") {
                        return;
                    }
                    self.id_to_name.insert(id, name.to_string());
                    self.id_to_start_ms.insert(id, time_ms);
                    self.total_started += 1;
                }
            }
            "error" => {
                if let Some(id) = val.get("testID").and_then(Value::as_i64) {
                    let err_msg = val
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("Test failed");
                    let stack = val
                        .get("stackTrace")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let first_stack_line = stack
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or_default();
                    let combined = if first_stack_line.is_empty() {
                        err_msg.trim().to_string()
                    } else {
                        format!("{}\n{}", err_msg.trim(), first_stack_line.trim())
                    };
                    self.id_to_errors.entry(id).or_default().push(combined);
                }
            }
            "testDone" => {
                let hidden = val.get("hidden").and_then(Value::as_bool).unwrap_or(false);
                if hidden {
                    return;
                }
                if let Some(id) = val.get("testID").and_then(Value::as_i64)
                    && let Some(name) = self.id_to_name.get(&id).cloned()
                {
                    self.total_done += 1;
                    let start = self.id_to_start_ms.get(&id).copied().unwrap_or(time_ms);
                    let duration_ms = time_ms.saturating_sub(start);
                    let skipped = val.get("skipped").and_then(Value::as_bool).unwrap_or(false);
                    let result = val.get("result").and_then(Value::as_str).unwrap_or("success");

                    let status = if skipped {
                        TestStatus::Skipped
                    } else if result == "success" {
                        TestStatus::Passed { duration_ms }
                    } else {
                        self.failed_count += 1;
                        let err = self
                            .id_to_errors
                            .get(&id)
                            .map(|v| v.join("\n"))
                            .unwrap_or_else(|| "Test failed".to_string());
                        TestStatus::Failed {
                            duration_ms,
                            error: err,
                        }
                    };
                    self.outcomes.insert(name, status);
                }
            }
            _ => {}
        }
    }
}

/// Converts failed test outcomes into LSP `Diagnostic`s placed on the matching `test()` lines.
pub fn build_test_diagnostics(
    tests: &[DartTestEntry],
    outcomes: &HashMap<String, TestStatus>,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for entry in tests {
        if entry.is_group {
            continue;
        }
        let status = outcomes
            .get(&entry.full_name)
            .or_else(|| outcomes.get(&entry.local_name));
        if let Some(TestStatus::Failed { error, .. }) = status {
            diags.push(Diagnostic {
                range: Range {
                    start: Position::new(entry.line, 0),
                    end: Position::new(entry.line, 80),
                },
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("flutter-test".to_string()),
                message: format!("Test `{}` failed:\n{error}", entry.local_name),
                ..Default::default()
            });
        }
    }
    diags
}

/// Executes `flutter test --machine` (or `dart test --reporter json`) in the background,
/// streaming live `$/progress`, updating [`TestRunStore`], and publishing failure diagnostics.
#[allow(clippy::too_many_arguments)]
pub async fn run_tests_streaming(
    client: Client,
    store: TestRunStore,
    uri: Uri,
    project: ProjectInfo,
    relative_path: String,
    test_name: Option<String>,
    device_id: Option<String>,
    custom_sdk_path: Option<String>,
    parsed_tests: Vec<DartTestEntry>,
) {
    let exe = resolve_sdk_executable(&project.root, project.kind, custom_sdk_path.as_deref());
    let mut args = vec!["test".to_string()];
    if project.kind == ProjectKind::Flutter {
        args.push("--machine".to_string());
    } else {
        args.push("--reporter".to_string());
        args.push("json".to_string());
    }
    args.push(relative_path.clone());

    if let Some(ref name) = test_name
        && !name.trim().is_empty()
    {
        args.push("--plain-name".to_string());
        args.push(name.clone());
    }
    if let Some(ref dev) = device_id
        && !dev.trim().is_empty()
    {
        args.push("-d".to_string());
        args.push(dev.clone());
    }

    let progress = ProgressHandle::begin(
        client.clone(),
        "Flutter Test",
        Some(format!("Running {relative_path}...")),
    )
    .await;

    let mut child = match Command::new(&exe)
        .args(&args)
        .current_dir(&project.root)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(err) => {
            progress.end(Some("Failed to start".to_string())).await;
            client
                .show_message(MessageType::ERROR, format!("Failed to spawn `{exe}`: {err}"))
                .await;
            return;
        }
    };

    let Some(stdout) = child.stdout.take() else {
        return;
    };

    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let mut parser = MachineTestParser::default();

    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
        parser.feed_line(&line);
        if parser.total_started > 0 {
            progress
                .report(format!(
                    "Running tests ({}/{})...",
                    parser.total_done, parser.total_started
                ))
                .await;
        }
        line.clear();
    }

    let _ = child.wait().await;

    let replace_all = test_name.is_none();
    store.update_file_results(&uri, parser.outcomes.clone(), replace_all);

    let merged = store.get_file_results(&uri);
    let diags = build_test_diagnostics(&parsed_tests, &merged);
    client.publish_diagnostics(uri.clone(), diags, None).await;
    let _ = client.send_request::<CodeLensRefresh>(()).await;

    let summary = if parser.failed_count == 0 {
        format!("Passed ({} test(s))", parser.total_done)
    } else {
        format!(
            "Failed ({} failed / {} total)",
            parser.failed_count, parser.total_done
        )
    };
    progress.end(Some(summary)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flutter_test_machine_stream_and_builds_failure_diagnostics() {
        let stream = [
            r#"{"type":"testStart","test":{"id":1,"name":"loading test/a_test.dart"},"time":10}"#,
            r#"{"type":"testStart","test":{"id":2,"name":"Counter increments"},"time":50}"#,
            r#"{"type":"testDone","testID":2,"result":"success","hidden":false,"skipped":false,"time":134}"#,
            r#"{"type":"testStart","test":{"id":3,"name":"Counter decrements"},"time":140}"#,
            r#"{"type":"error","testID":3,"error":"Expected: <0>\n  Actual: <1>","stackTrace":"test/a_test.dart 12:5  main.<fn>","isFailure":true,"time":160}"#,
            r#"{"type":"testDone","testID":3,"result":"failure","hidden":false,"skipped":false,"time":165}"#,
        ];

        let mut parser = MachineTestParser::default();
        for line in stream {
            parser.feed_line(line);
        }

        assert_eq!(parser.total_done, 2);
        assert_eq!(parser.failed_count, 1);
        assert_eq!(
            parser.outcomes.get("Counter increments"),
            Some(&TestStatus::Passed { duration_ms: 84 })
        );

        let entries = vec![
            DartTestEntry {
                line: 5,
                local_name: "increments".into(),
                full_name: "Counter increments".into(),
                is_group: false,
            },
            DartTestEntry {
                line: 10,
                local_name: "decrements".into(),
                full_name: "Counter decrements".into(),
                is_group: false,
            },
        ];

        let diags = build_test_diagnostics(&entries, &parser.outcomes);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].range.start.line, 10);
        assert!(diags[0].message.contains("Expected: <0>"));
    }
}
