use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::json;
use tower_lsp_server::ls_types::{CodeLens, Command, Position, Range, Uri};

use crate::app_runner::{FlutterRunMode, ManagedAppSnapshot};
use crate::ipc::ActiveSessionRecord;
use crate::project::ProjectKind;

/// Classifies a `.dart` file according to Dart / Flutter official testing conventions
/// (`test/`, `integration_test/`, `test_driver/`, `*_test.dart`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DartFileRole {
    /// Regular application or CLI entrypoint (`lib/main.dart`, `bin/main.dart`, etc.).
    #[default]
    AppOrScript,
    /// Unit or widget test file (`test/**/*_test.dart` or importing `package:test`/`flutter_test`).
    UnitOrWidgetTest,
    /// Flutter integration test file (`integration_test/**/*_test.dart`).
    IntegrationTest,
    /// Non-runnable helper/fixture inside a test folder (e.g. `test/helpers/mock_api.dart`).
    TestHelper,
}

impl DartFileRole {
    pub fn is_runnable_test(self) -> bool {
        matches!(self, Self::UnitOrWidgetTest | Self::IntegrationTest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DartTestEntry {
    pub line: u32,
    pub name: String,
    pub is_group: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedDartFile {
    pub role: DartFileRole,
    pub main_line: Option<u32>,
    pub tests: Vec<DartTestEntry>,
}

/// Classifies a Dart file using both its path relative to the package root and its imports/body.
///
/// Rules follow Dart `package:test`, Flutter `flutter_test` / `integration_test`, and VS Code
/// Dart-Code conventions:
/// - Files inside `integration_test/` ending in `_test.dart` (or importing `integration_test` /
///   `flutter_test` / `patrol`) are `IntegrationTest`.
/// - Files inside `test/` or `test_driver/` ending in `_test.dart` (or importing a test package)
///   are `UnitOrWidgetTest`; other files inside those folders are `TestHelper` (never shown as
///   runnable Flutter apps).
/// - Files outside test folders ending in `_test.dart` that import a test package are also
///   recognized as `UnitOrWidgetTest`.
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

    let imports_integration_test = content.contains("package:integration_test/")
        || content.contains("package:patrol/");
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

/// Synchronously scans a Dart file for `main()` and `test`/`testWidgets`/`group` declarations.
pub fn parse_dart_file(content: &str, role: DartFileRole) -> ParsedDartFile {
    static MAIN_RE: OnceLock<Regex> = OnceLock::new();
    static TEST_RE: OnceLock<Regex> = OnceLock::new();

    let main_re = MAIN_RE.get_or_init(|| {
        Regex::new(r"^\s*(?:Future\s*<\s*void\s*>\s+|void\s+)?main\s*\(")
            .expect("valid main regex")
    });
    let test_re = TEST_RE.get_or_init(|| {
        Regex::new(r#"^\s*(testWidgets|test|group)\s*\(\s*r?['"]([^'"]+)['"]"#)
            .expect("valid test regex")
    });

    let mut main_line = None;
    let mut tests = Vec::new();
    let scan_tests = role.is_runnable_test();

    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('*') {
            continue;
        }

        if main_line.is_none() && main_re.is_match(line) {
            main_line = Some(idx as u32);
        }

        if scan_tests
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

    ParsedDartFile {
        role,
        main_line,
        tests,
    }
}

/// Builds plain-text CodeLens entries for a Dart source or test file.
///
/// - For runnable test files (`UnitOrWidgetTest` / `IntegrationTest`), `main()` shows `Run Tests`
///   (and `Device: ...` / `Refresh Devices` only for Flutter `IntegrationTest` files) — never
///   app-launch `Debug` / `Profile` / `Release` buttons.
/// - For `TestHelper` files inside `test/`, no app-launch buttons are shown.
/// - For Flutter `AppOrScript` files (`lib/main.dart`, etc.), `main()` exposes `Debug`, `Profile`,
///   and `Release` (or active-session controls `Hot Reload`, `Hot Restart`, `DevTools`/`Performance`,
///   `Stop`) plus target device selection.
pub fn build_dart_code_lenses(
    uri: &Uri,
    parsed: &ParsedDartFile,
    project_kind: ProjectKind,
    selected_device_label: Option<&str>,
    dap_session: Option<&ActiveSessionRecord>,
    managed_app: Option<&ManagedAppSnapshot>,
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
                    "Run Tests",
                    "flutterDebugger.runTest",
                    vec![json!(uri_str)],
                ));

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
                    lenses.push(make_lens(
                        range,
                        "Refresh Devices",
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
                    "Run Group"
                } else {
                    "Run Test"
                };
                lenses.push(make_lens(
                    range,
                    label,
                    "flutterDebugger.runTest",
                    vec![json!(uri_str), json!(entry.name)],
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

                let active_mode = managed_app
                    .map(|m| m.mode.as_str())
                    .or_else(|| dap_session.map(|s| s.mode.as_str()));

                let has_devtools = managed_app
                    .is_some_and(|m| m.devtools_uri.is_some() || m.vm_service_uri.is_some())
                    || dap_session
                        .is_some_and(|s| s.devtools_uri.is_some() || s.vm_service_uri.is_some());

                if let Some(mode) = active_mode {
                    if mode == "debug" {
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
                    if has_devtools {
                        let dt_label = if mode == "profile" {
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
                    if managed_app.is_some() {
                        lenses.push(make_lens(
                            range,
                            "Stop",
                            "flutterDebugger.stopApp",
                            vec![json!(uri_str)],
                        ));
                    }
                } else if project_kind == ProjectKind::Flutter {
                    lenses.push(make_lens(
                        range,
                        "Debug",
                        "flutterDebugger.runMode",
                        vec![json!(uri_str), json!(FlutterRunMode::Debug.as_str())],
                    ));
                    lenses.push(make_lens(
                        range,
                        "Profile",
                        "flutterDebugger.runMode",
                        vec![json!(uri_str), json!(FlutterRunMode::Profile.as_str())],
                    ));
                    lenses.push(make_lens(
                        range,
                        "Release",
                        "flutterDebugger.runMode",
                        vec![json!(uri_str), json!(FlutterRunMode::Release.as_str())],
                    ));
                }

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
                    lenses.push(make_lens(
                        range,
                        "Refresh Devices",
                        "flutterDebugger.refreshDevices",
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

/// Maps `lib/<rel>.dart` <-> `test/<rel>_test.dart` when the counterpart file exists on disk.
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
    fn classifies_unit_integration_helper_and_app_files() {
        let root = Path::new("/repo");
        assert_eq!(
            classify_dart_file_role(
                Path::new("/repo/test/widget_test.dart"),
                Some(root),
                "import 'package:flutter_test/flutter_test.dart';\nvoid main() {}",
            ),
            DartFileRole::UnitOrWidgetTest
        );
        assert_eq!(
            classify_dart_file_role(
                Path::new("/repo/integration_test/app_test.dart"),
                Some(root),
                "import 'package:integration_test/integration_test.dart';\nvoid main() {}",
            ),
            DartFileRole::IntegrationTest
        );
        assert_eq!(
            classify_dart_file_role(
                Path::new("/repo/test/helpers/fake_client.dart"),
                Some(root),
                "class FakeClient {}",
            ),
            DartFileRole::TestHelper
        );
        assert_eq!(
            classify_dart_file_role(
                Path::new("/repo/lib/main.dart"),
                Some(root),
                "import 'package:flutter/material.dart';\nvoid main() {}",
            ),
            DartFileRole::AppOrScript
        );
    }

    #[test]
    fn test_file_main_shows_run_tests_and_never_app_launchers() {
        let code = r#"
import 'package:flutter_test/flutter_test.dart';

Future<void> main() async {
  group('Counter', () {
    testWidgets('increments counter', (tester) async {});
    test('adds numbers', () {});
  });
}
"#;
        let parsed = parse_dart_file(code, DartFileRole::UnitOrWidgetTest);
        assert_eq!(parsed.main_line, Some(3));
        assert_eq!(parsed.tests.len(), 3);

        let uri: Uri = "file:///tmp/app/test/widget_test.dart".parse().expect("uri");
        let lenses = build_dart_code_lenses(&uri, &parsed, ProjectKind::Flutter, None, None, None);
        let titles: Vec<&str> = lenses
            .iter()
            .filter_map(|l| l.command.as_ref().map(|c| c.title.as_str()))
            .collect();
        assert_eq!(titles, vec!["Run Tests", "Run Group", "Run Test", "Run Test"]);
        assert!(!titles.contains(&"Debug"));
        assert!(!titles.contains(&"Profile"));
        assert!(!titles.contains(&"Release"));
    }

    #[test]
    fn integration_test_main_shows_run_tests_and_device_selector() {
        let code = r#"
import 'package:integration_test/integration_test.dart';
void main() {
  testWidgets('full flow', (tester) async {});
}
"#;
        let parsed = parse_dart_file(code, DartFileRole::IntegrationTest);
        let uri: Uri = "file:///tmp/app/integration_test/app_test.dart"
            .parse()
            .expect("uri");
        let lenses = build_dart_code_lenses(&uri, &parsed, ProjectKind::Flutter, None, None, None);
        let titles: Vec<&str> = lenses
            .iter()
            .filter_map(|l| l.command.as_ref().map(|c| c.title.as_str()))
            .collect();
        assert_eq!(
            titles,
            vec!["Run Tests", "Device: Auto", "Refresh Devices", "Run Test"]
        );
        assert!(!titles.contains(&"Debug"));
    }

    #[test]
    fn shows_debug_profile_release_lenses_on_flutter_app_main() {
        let parsed = parse_dart_file(
            "void main() => runApp(const App());",
            DartFileRole::AppOrScript,
        );
        let uri: Uri = "file:///tmp/app/lib/main.dart".parse().expect("uri");
        let lenses = build_dart_code_lenses(&uri, &parsed, ProjectKind::Flutter, None, None, None);
        let titles: Vec<&str> = lenses
            .iter()
            .filter_map(|l| l.command.as_ref().map(|c| c.title.as_str()))
            .collect();
        assert!(titles.contains(&"Debug"));
        assert!(titles.contains(&"Profile"));
        assert!(titles.contains(&"Release"));
    }

    #[test]
    fn maps_lib_and_test_counterparts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lib_file = dir.path().join("lib").join("counter.dart");
        let test_file = dir.path().join("test").join("counter_test.dart");
        std::fs::create_dir_all(lib_file.parent().expect("lib")).expect("mkdir lib");
        std::fs::create_dir_all(test_file.parent().expect("test")).expect("mkdir test");
        std::fs::write(&lib_file, "class Counter {}").expect("write lib");
        std::fs::write(&test_file, "void main() {}").expect("write test");

        assert_eq!(
            find_counterpart_test_or_impl(&lib_file, dir.path()),
            Some((test_file.clone(), true))
        );
        assert_eq!(
            find_counterpart_test_or_impl(&test_file, dir.path()),
            Some((lib_file, false))
        );
    }
}
