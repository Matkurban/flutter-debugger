use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;

use dashmap::DashMap;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;
use tower_lsp_server::Client;
use tower_lsp_server::ls_types::{
    Diagnostic, DiagnosticSeverity, MessageType, Position, Range, Uri, request::CodeLensRefresh,
};

use crate::lsp::dart_file::DartTestEntry;
use crate::lsp::progress::ProgressHandle;
use crate::process_util::build_command;
use crate::project::{ProjectInfo, ProjectKind, resolve_sdk_executable};
use crate::runner::{extract_error_summary, strip_ansi_codes};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestStatus {
    Passed { duration_ms: u64 },
    Failed { duration_ms: u64, error: String },
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestFilter {
    All,
    PlainName(String),
    NameRegex(String),
}

/// Tracks per-test outcomes and active per-URI test tasks (allowing clean cancellation
/// when re-running tests on the same file).
#[derive(Debug, Clone, Default)]
pub struct TestRunStore {
    by_file: Arc<DashMap<String, HashMap<String, TestStatus>>>,
    active_runs: Arc<DashMap<String, tokio::task::AbortHandle>>,
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

    pub fn update_file_results(
        &self,
        uri: &Uri,
        updates: HashMap<String, TestStatus>,
        replace: bool,
    ) {
        if replace {
            self.by_file.insert(uri.as_str().to_string(), updates);
        } else {
            let mut entry = self.by_file.entry(uri.as_str().to_string()).or_default();
            for (k, v) in updates {
                entry.insert(k, v);
            }
        }
    }

    /// Cancels any previous in-flight test run for `uri` and registers the new task handle.
    pub fn register_active_run(&self, uri: &Uri, handle: tokio::task::AbortHandle) {
        if let Some(prev) = self.active_runs.insert(uri.as_str().to_string(), handle) {
            prev.abort();
        }
    }

    pub fn clear_active_run(&self, uri: &Uri) {
        self.active_runs.remove(uri.as_str());
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
    pub plain_output: Vec<String>,
}

impl MachineTestParser {
    pub fn feed_line(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        if !trimmed.starts_with('{') {
            self.plain_output.push(strip_ansi_codes(trimmed));
            return;
        }
        let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
            self.plain_output.push(strip_ansi_codes(trimmed));
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
                    let result = val
                        .get("result")
                        .and_then(Value::as_str)
                        .unwrap_or("success");

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

/// Evaluates final progress summary & optional file-level compilation/setup diagnostic
/// when the test runner exits with a non-zero status and zero completed tests.
pub fn summarize_test_exit(
    exit_code: i32,
    parser: &MachineTestParser,
    stderr: &str,
    main_line: Option<u32>,
    source_text: &str,
) -> (String, Option<Diagnostic>) {
    if exit_code != 0 && parser.total_done == 0 {
        let plain_stdout = parser.plain_output.join("\n");
        let summary = extract_error_summary(stderr, &plain_stdout, exit_code);
        let target_line = main_line.unwrap_or(0);
        let end_col = utf16_line_len(source_text, target_line);
        let diag = Diagnostic {
            range: Range {
                start: Position::new(target_line, 0),
                end: Position::new(target_line, end_col),
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("flutter-test".to_string()),
            message: format!("Test run failed before executing tests:\n{summary}"),
            ..Default::default()
        };
        return ("Failed (compilation/setup error)".to_string(), Some(diag));
    }

    if parser.failed_count == 0 && exit_code == 0 {
        (format!("Passed ({} test(s))", parser.total_done), None)
    } else {
        (
            format!(
                "Failed ({} failed / {} total)",
                parser.failed_count, parser.total_done
            ),
            None,
        )
    }
}

/// Converts failed test outcomes into LSP `Diagnostic`s placed on the matching `test()` lines
/// with exact UTF-16 line end columns.
pub fn build_test_diagnostics(
    tests: &[DartTestEntry],
    outcomes: &HashMap<String, TestStatus>,
    source_text: &str,
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
            let end_col = utf16_line_len(source_text, entry.line);
            diags.push(Diagnostic {
                range: Range {
                    start: Position::new(entry.line, 0),
                    end: Position::new(entry.line, end_col),
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

fn utf16_line_len(source: &str, line_idx: u32) -> u32 {
    source
        .lines()
        .nth(line_idx as usize)
        .map(|l| l.trim_end_matches('\r').encode_utf16().count() as u32)
        .unwrap_or(1)
        .max(1)
}

/// Escapes regex metacharacters and joins multiple test names into a single `--name` pattern.
pub fn build_multi_test_regex(names: &[String]) -> String {
    names
        .iter()
        .map(|n| format!("^{}$", regex::escape(n)))
        .collect::<Vec<_>>()
        .join("|")
}

/// Executes `flutter test --machine` (or `dart test --reporter json`) in the background,
/// draining stderr concurrently to prevent pipe deadlocks, checking exit codes for compile
/// errors, and publishing failure diagnostics.
#[allow(clippy::too_many_arguments)]
pub async fn run_tests_streaming(
    client: Client,
    store: TestRunStore,
    uri: Uri,
    project: ProjectInfo,
    relative_path: String,
    filter: TestFilter,
    device_id: Option<String>,
    custom_sdk_path: Option<String>,
    parsed_tests: Vec<DartTestEntry>,
    main_line: Option<u32>,
    source_text: String,
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

    match &filter {
        TestFilter::All => {}
        TestFilter::PlainName(name) if !name.trim().is_empty() => {
            args.push("--plain-name".to_string());
            args.push(name.clone());
        }
        TestFilter::NameRegex(pattern) if !pattern.trim().is_empty() => {
            args.push("--name".to_string());
            args.push(pattern.clone());
        }
        _ => {}
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

    let mut child = match build_command(&exe, &args, &project.root)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(err) => {
            store.clear_active_run(&uri);
            progress.end(Some("Failed to start".to_string())).await;
            client
                .show_message(MessageType::ERROR, format!("Failed to spawn `{exe}`: {err}"))
                .await;
            return;
        }
    };

    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        store.clear_active_run(&uri);
        return;
    };

    // Concurrently drain stderr so a verbose compiler/test error can NEVER deadlock the 64KB pipe
    let stderr_buf = Arc::new(Mutex::new(String::new()));
    let stderr_buf_clone = Arc::clone(&stderr_buf);
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
            let cleaned = strip_ansi_codes(&line);
            stderr_buf_clone.lock().await.push_str(&cleaned);
            line.clear();
        }
    });

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

    let exit_code = child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1);
    let _ = stderr_task.await;
    let stderr_text = stderr_buf.lock().await.clone();

    let replace_all = matches!(filter, TestFilter::All);
    store.update_file_results(&uri, parser.outcomes.clone(), replace_all);

    let merged = store.get_file_results(&uri);
    let mut diags = build_test_diagnostics(&parsed_tests, &merged, &source_text);
    let (summary, compile_diag) =
        summarize_test_exit(exit_code, &parser, &stderr_text, main_line, &source_text);

    if let Some(cd) = compile_diag {
        client
            .show_message(MessageType::ERROR, cd.message.clone())
            .await;
        diags.push(cd);
    }

    client.publish_diagnostics(uri.clone(), diags, None).await;
    let _ = client.send_request::<CodeLensRefresh>(()).await;

    progress.end(Some(summary)).await;
    store.clear_active_run(&uri);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_compilation_failure_when_exit_nonzero_and_zero_tests_done() {
        let parser = MachineTestParser::default();
        let src = "void main() {\n  test('a', () {});\n}\n";
        let (summary, diag) = summarize_test_exit(
            1,
            &parser,
            "Error: Expected ';' after this.\n",
            Some(0),
            src,
        );
        assert!(summary.contains("Failed"));
        let d = diag.expect("compilation diagnostic");
        assert_eq!(d.range.start.line, 0);
        assert_eq!(d.range.end.character, 13);
        assert!(d.message.contains("Expected ';'"));
    }

    #[test]
    fn escapes_and_joins_multiple_failed_test_names_into_regex() {
        let regex = build_multi_test_regex(&[
            "Cart adds $10 (USD)".into(),
            "Cart removes item".into(),
        ]);
        assert_eq!(regex, r"^Cart adds \$10 \(USD\)$|^Cart removes item$");
    }

    #[test]
    fn uses_exact_utf16_line_length_for_test_diagnostics() {
        let src = "void main() {\n  test('测试失败', () {});\n}\n";
        let entries = vec![DartTestEntry {
            line: 1,
            local_name: "测试失败".into(),
            full_name: "测试失败".into(),
            is_group: false,
        }];
        let mut outcomes = HashMap::new();
        outcomes.insert(
            "测试失败".into(),
            TestStatus::Failed {
                duration_ms: 10,
                error: "mismatch".into(),
            },
        );
        let diags = build_test_diagnostics(&entries, &outcomes, src);
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].range.end.character,
            "  test('测试失败', () {});".encode_utf16().count() as u32
        );
    }
}
