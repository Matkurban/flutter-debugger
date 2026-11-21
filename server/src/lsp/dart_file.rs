use std::sync::OnceLock;

use regex::Regex;
use serde_json::json;
use tower_lsp_server::ls_types::{CodeLens, Command, Position, Range, Uri};

use crate::ipc::ActiveSessionRecord;
use crate::project::ProjectKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DartTestEntry {
    pub line: u32,
    pub name: String,
    pub is_group: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedDartFile {
    pub main_line: Option<u32>,
    pub tests: Vec<DartTestEntry>,
}

/// Synchronously scans a Dart file for `main()` and `test`/`testWidgets`/`group` declarations.
pub fn parse_dart_file(content: &str, is_test_file: bool) -> ParsedDartFile {
    static MAIN_RE: OnceLock<Regex> = OnceLock::new();
    static TEST_RE: OnceLock<Regex> = OnceLock::new();

    let main_re = MAIN_RE.get_or_init(|| {
        Regex::new(r"^\s*(?:Future\s*<\s*void\s*>\s+|void\s+)?main\s*\(")
            .expect("valid main regex")
    });
    let test_re = TEST_RE.get_or_init(|| {
        Regex::new(r#"^\s*(testWidgets|test|group)\s*\(\s*['"]([^'"]+)['"]"#)
            .expect("valid test regex")
    });

    let mut main_line = None;
    let mut tests = Vec::new();

    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('*') {
            continue;
        }

        if main_line.is_none() && main_re.is_match(line) {
            main_line = Some(idx as u32);
        }

        if is_test_file
            && let Some(caps) = test_re.captures(line)
            && let (Some(kind), Some(name)) = (caps.get(1), caps.get(2))
        {
            tests.push(DartTestEntry {
                line: idx as u32,
                name: name.as_str().to_string(),
                is_group: kind.as_str() == "group",
            });
        }
    }

    ParsedDartFile { main_line, tests }
}

/// Builds monochrome Unicode CodeLens entries for a Dart source or test file.
pub fn build_dart_code_lenses(
    uri: &Uri,
    parsed: &ParsedDartFile,
    project_kind: ProjectKind,
    selected_device_label: Option<&str>,
    active_session: Option<&ActiveSessionRecord>,
) -> Vec<CodeLens> {
    let mut lenses = Vec::new();
    let uri_str = uri.as_str().to_string();

    if let Some(line) = parsed.main_line {
        let range = Range {
            start: Position::new(line, 0),
            end: Position::new(line, 0),
        };

        if let Some(session) = active_session {
            if session.mode == "debug" {
                lenses.push(make_lens(
                    range,
                    "⟳ Hot Reload",
                    "flutterDebugger.hotReload",
                    vec![json!(uri_str)],
                ));
                lenses.push(make_lens(
                    range,
                    "↻ Hot Restart",
                    "flutterDebugger.hotRestart",
                    vec![json!(uri_str)],
                ));
            }
            if session.devtools_uri.is_some() || session.vm_service_uri.is_some() {
                lenses.push(make_lens(
                    range,
                    "⊞ Open DevTools",
                    "flutterDebugger.openDevTools",
                    vec![json!(uri_str)],
                ));
            }
        }

        if project_kind == ProjectKind::Flutter {
            let device_title = match selected_device_label {
                Some(label) => format!("◎ Device: {label}"),
                None => "◎ Device: Auto".to_string(),
            };
            lenses.push(make_lens(
                range,
                &device_title,
                "flutterDebugger.cycleDevice",
                vec![json!(uri_str)],
            ));
            lenses.push(make_lens(
                range,
                "↻ Refresh Devices",
                "flutterDebugger.refreshDevices",
                vec![json!(uri_str)],
            ));
        }
    }

    for entry in &parsed.tests {
        let range = Range {
            start: Position::new(entry.line, 0),
            end: Position::new(entry.line, 0),
        };
        let label = if entry.is_group {
            "▷ Run Group"
        } else {
            "▷ Run Test"
        };
        lenses.push(make_lens(
            range,
            label,
            "flutterDebugger.runTest",
            vec![json!(uri_str), json!(entry.name)],
        ));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_main_and_widget_tests() {
        let code = r#"
import 'package:flutter_test/flutter_test.dart';

Future<void> main() async {
  group('Counter', () {
    testWidgets('increments counter', (tester) async {});
    test('adds numbers', () {});
  });
}
"#;
        let parsed = parse_dart_file(code, true);
        assert_eq!(parsed.main_line, Some(3));
        assert_eq!(parsed.tests.len(), 3);
        assert!(parsed.tests[0].is_group);
        assert_eq!(parsed.tests[0].name, "Counter");
        assert_eq!(parsed.tests[1].name, "increments counter");
        assert_eq!(parsed.tests[2].name, "adds numbers");
    }
}
