use std::collections::HashMap;
use std::path::Path;

use serde_json::json;
use tower_lsp_server::ls_types::{CodeLens, Command, Position, Range, Uri};

use crate::ipc::ActiveSessionRecord;
use crate::lsp::dart_parser::{ClosingLabelEntry, parse_dart_syntax};
use crate::lsp::test_runner::TestStatus;
use crate::project::ProjectKind;

pub use crate::lsp::dart_parser::DartTestEntry;

/// Classifies a `.dart` file according to Dart / Flutter official testing conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DartFileRole {
    #[default]
    AppOrScript,
    UnitOrWidgetTest,
    IntegrationTest,
    TestHelper,
}

impl DartFileRole {
    pub fn is_runnable_test(self) -> bool {
        matches!(self, Self::UnitOrWidgetTest | Self::IntegrationTest)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ParsedDartFile {
    pub role: DartFileRole,
    pub main_line: Option<u32>,
    pub tests: Vec<DartTestEntry>,
    pub closing_labels: Vec<ClosingLabelEntry>,
}

pub fn classify_dart_file_role(
    file_path: &Path,
    project_root: Option<&Path>,
    content: &str,
) -> DartFileRole {
    let rel_path = project_root
        .and_then(|root| file_path.strip_prefix(root).ok())
        .unwrap_or(file_path);

    let components: Vec<&str> = rel_path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();

    let file_name = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let ends_with_test = file_name.ends_with("_test.dart");

    let in_integration_test = components.contains(&"integration_test");
    let in_test_folder = components.contains(&"test") || components.contains(&"test_driver");

    let imports_integration_test =
        content.contains("package:integration_test/") || content.contains("package:patrol/");
    let imports_test_pkg = imports_integration_test
        || content.contains("package:flutter_test/")
        || content.contains("package:test/")
        || content.contains("package:test_api/");

    if in_integration_test {
        if ends_with_test || imports_test_pkg {
            return DartFileRole::IntegrationTest;
        }
        return DartFileRole::TestHelper;
    }

    if in_test_folder {
        if ends_with_test || imports_test_pkg {
            return DartFileRole::UnitOrWidgetTest;
        }
        return DartFileRole::TestHelper;
    }

    if ends_with_test && imports_test_pkg {
        return if imports_integration_test {
            DartFileRole::IntegrationTest
        } else {
            DartFileRole::UnitOrWidgetTest
        };
    }

    DartFileRole::AppOrScript
}

/// Parses a Dart file using the comment/string-aware balanced delimiter parser.
pub fn parse_dart_file(content: &str, role: DartFileRole) -> ParsedDartFile {
    let syntax = parse_dart_syntax(content, role.is_runnable_test());
    ParsedDartFile {
        role,
        main_line: syntax.main_line,
        tests: syntax.tests,
        closing_labels: syntax.closing_labels,
    }
}

/// Returns `true` if a saved `.dart` file should NOT trigger auto Hot Reload.
pub fn should_skip_auto_hot_reload(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if name.ends_with("_test.dart")
        || name.ends_with(".g.dart")
        || name.ends_with(".freezed.dart")
        || name.ends_with(".gr.dart")
        || name.ends_with(".mocks.dart")
        || name.ends_with(".gen.dart")
    {
        return true;
    }
    let norm = path.to_string_lossy().replace('\\', "/");
    norm.contains("/test/") || norm.contains("/integration_test/")
}

pub fn build_dart_code_lenses(
    uri: &Uri,
    parsed: &ParsedDartFile,
    project_kind: ProjectKind,
    selected_device_label: Option<&str>,
    active_session: Option<&ActiveSessionRecord>,
    test_results: &HashMap<String, TestStatus>,
) -> Vec<CodeLens> {
    let mut lenses = Vec::new();
    let uri_str = uri.as_str().to_string();

    match parsed.role {
        DartFileRole::UnitOrWidgetTest | DartFileRole::IntegrationTest => {
            if let Some(line) = parsed.main_line {
                let range = Range {
                    start: Position::new(line, 0),
                    end: Position::new(line, 0),
                };
                lenses.push(make_lens(
                    range,
                    "Run File",
                    "flutterDebugger.runTest",
                    vec![json!(uri_str)],
                ));

                // Collect all failed tests in this file for one-click Re-run Failed
                let failed_names: Vec<String> = parsed
                    .tests
                    .iter()
                    .filter(|t| {
                        !t.is_group
                            && matches!(
                                test_results
                                    .get(&t.full_name)
                                    .or_else(|| test_results.get(&t.local_name)),
                                Some(TestStatus::Failed { .. })
                            )
                    })
                    .map(|t| t.full_name.clone())
                    .collect();

                if failed_names.len() == 1 {
                    lenses.push(make_lens(
                        range,
                        "Re-run Failed",
                        "flutterDebugger.runTest",
                        vec![json!(uri_str), json!(failed_names[0]), json!("plain")],
                    ));
                } else if failed_names.len() > 1 {
                    let pattern = crate::lsp::test_runner::build_multi_test_regex(&failed_names);
                    lenses.push(make_lens(
                        range,
                        &format!("Re-run Failed ({})", failed_names.len()),
                        "flutterDebugger.runTest",
                        vec![json!(uri_str), json!(pattern), json!("regex")],
                    ));
                }

                if parsed.role == DartFileRole::IntegrationTest
                    && project_kind == ProjectKind::Flutter
                {
                    let device_title = match selected_device_label {
                        Some(label) => format!("Device: {label}"),
                        None => "Device: Auto".to_string(),
                    };
                    lenses.push(make_lens(
                        range,
                        &device_title,
                        "flutterDebugger.cycleDevice",
                        vec![json!(uri_str)],
                    ));
                }
            }

            for entry in &parsed.tests {
                let range = Range {
                    start: Position::new(entry.line, 0),
                    end: Position::new(entry.line, 0),
                };
                let base_label = if entry.is_group {
                    "Run Group"
                } else {
                    "Run Test"
                };
                let label = if entry.is_group {
                    base_label.to_string()
                } else {
                    match test_results
                        .get(&entry.full_name)
                        .or_else(|| test_results.get(&entry.local_name))
                    {
                        Some(TestStatus::Passed { duration_ms }) => {
                            format!("{base_label} (PASS {duration_ms}ms)")
                        }
                        Some(TestStatus::Failed { .. }) => format!("{base_label} (FAIL)"),
                        Some(TestStatus::Skipped) => format!("{base_label} (SKIP)"),
                        None => base_label.to_string(),
                    }
                };

                lenses.push(make_lens(
                    range,
                    &label,
                    "flutterDebugger.runTest",
                    vec![json!(uri_str), json!(entry.full_name)],
                ));
            }
        }
        DartFileRole::TestHelper => {}
        DartFileRole::AppOrScript => {
            if let Some(line) = parsed.main_line {
                let range = Range {
                    start: Position::new(line, 0),
                    end: Position::new(line, 0),
                };

                if project_kind == ProjectKind::Flutter {
                    let device_title = match selected_device_label {
                        Some(label) => format!("Device: {label}"),
                        None => "Device: Auto".to_string(),
                    };
                    lenses.push(make_lens(
                        range,
                        &device_title,
                        "flutterDebugger.cycleDevice",
                        vec![json!(uri_str)],
                    ));
                }

                if let Some(session) = active_session {
                    if session.mode == "debug" {
                        lenses.push(make_lens(
                            range,
                            "Hot Reload",
                            "flutterDebugger.hotReload",
                            vec![json!(uri_str)],
                        ));
                        lenses.push(make_lens(
                            range,
                            "Hot Restart",
                            "flutterDebugger.hotRestart",
                            vec![json!(uri_str)],
                        ));
                    }
                    if session.devtools_uri.is_some() || session.vm_service_uri.is_some() {
                        let dt_label = if session.mode == "profile" {
                            "Performance"
                        } else {
                            "DevTools"
                        };
                        lenses.push(make_lens(
                            range,
                            dt_label,
                            "flutterDebugger.openDevTools",
                            vec![json!(uri_str)],
                        ));
                    }
                    lenses.push(make_lens(
                        range,
                        "Stop",
                        "flutterDebugger.stopApp",
                        vec![json!(uri_str)],
                    ));
                }
            }
        }
    }

    lenses
}

fn make_lens(range: Range, title: &str, command: &str, arguments: Vec<serde_json::Value>) -> CodeLens {
    CodeLens {
        range,
        command: Some(Command {
            title: title.to_string(),
            command: command.to_string(),
            arguments: Some(arguments),
        }),
        data: None,
    }
}

pub fn find_counterpart_test_or_impl(
    file_path: &Path,
    project_root: &Path,
) -> Option<(std::path::PathBuf, bool)> {
    let rel = file_path.strip_prefix(project_root).ok()?;
    let rel_str = rel.to_string_lossy().replace('\\', "/");

    if let Some(rest) = rel_str.strip_prefix("lib/")
        && let Some(stem) = rest.strip_suffix(".dart")
    {
        let candidate = project_root
            .join("test")
            .join(format!("{stem}_test.dart"));
        if candidate.is_file() {
            return Some((candidate, true));
        }
    } else if let Some(rest) = rel_str.strip_prefix("test/")
        && let Some(stem) = rest.strip_suffix("_test.dart")
    {
        let candidate = project_root.join("lib").join(format!("{stem}.dart"));
        if candidate.is_file() {
            return Some((candidate, false));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_generated_and_test_files_for_auto_hot_reload() {
        assert!(should_skip_auto_hot_reload(Path::new("/app/test/foo_test.dart")));
        assert!(should_skip_auto_hot_reload(Path::new("/app/lib/user.g.dart")));
        assert!(should_skip_auto_hot_reload(Path::new("/app/lib/user.freezed.dart")));
        assert!(!should_skip_auto_hot_reload(Path::new("/app/lib/main.dart")));
    }

    #[test]
    fn test_lenses_reflect_nested_group_full_names_and_pass_fail_status() {
        let code = r#"
import 'package:flutter_test/flutter_test.dart';
void main() {
  group('Cart', () {
    test('adds item', () {});
    test('removes item', () {});
  });
}
"#;
        let parsed = parse_dart_file(code, DartFileRole::UnitOrWidgetTest);
        let uri: Uri = "file:///tmp/app/test/cart_test.dart".parse().expect("uri");
        let mut results = HashMap::new();
        results.insert(
            "Cart adds item".to_string(),
            TestStatus::Passed { duration_ms: 42 },
        );
        results.insert(
            "Cart removes item".to_string(),
            TestStatus::Failed {
                duration_ms: 15,
                error: "boom".into(),
            },
        );

        let lenses =
            build_dart_code_lenses(&uri, &parsed, ProjectKind::Flutter, None, None, &results);
        let titles: Vec<&str> = lenses
            .iter()
            .filter_map(|l| l.command.as_ref().map(|c| c.title.as_str()))
            .collect();
        assert_eq!(
            titles,
            vec![
                "Run File",
                "Re-run Failed",
                "Run Group",
                "Run Test (PASS 42ms)",
                "Run Test (FAIL)"
            ]
        );
    }
}
