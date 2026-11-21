use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Distinguishes a Flutter app/plugin from a pure Dart package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectKind {
    Flutter,
    Dart,
}

impl ProjectKind {
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::Flutter => "flutter",
            Self::Dart => "dart",
        }
    }
}

/// Resolved metadata for a Flutter or pure Dart package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInfo {
    pub root: PathBuf,
    pub pubspec_path: PathBuf,
    pub kind: ProjectKind,
}

/// Walks upward from `file_or_dir` to find the nearest `pubspec.yaml`.
///
/// When editing `pubspec_overrides.yaml`, this resolves the sibling `pubspec.yaml`
/// in the same directory so Flutter vs. Dart classification remains accurate.
pub fn find_project_for_path(file_or_dir: &Path) -> Option<ProjectInfo> {
    let start_dir = if file_or_dir.is_dir() {
        file_or_dir
    } else {
        file_or_dir.parent()?
    };

    let mut current = Some(start_dir);
    while let Some(dir) = current {
        let pubspec_path = dir.join("pubspec.yaml");
        if pubspec_path.is_file() {
            let content = fs::read_to_string(&pubspec_path).ok()?;
            let kind = classify_pubspec(&content);
            return Some(ProjectInfo {
                root: dir.to_path_buf(),
                pubspec_path,
                kind,
            });
        }
        current = dir.parent();
    }

    None
}

/// Classifies a `pubspec.yaml` document as Flutter vs. pure Dart.
///
/// A project is classified as [`ProjectKind::Flutter`] if any of the following holds:
/// 1. A top-level `flutter:` section exists.
/// 2. `environment:` contains a `flutter:` SDK constraint.
/// 3. Any dependency declares `sdk: flutter`.
pub fn classify_pubspec(content: &str) -> ProjectKind {
    let mut is_flutter = false;
    let mut current_section: Option<&str> = None;

    for raw_line in content.lines() {
        let line = strip_yaml_comment(raw_line);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let indent = raw_line.len() - raw_line.trim_start_matches(' ').len();

        if indent == 0 {
            if let Some(key) = extract_yaml_key(trimmed) {
                current_section = Some(key);
                if key == "flutter" {
                    is_flutter = true;
                }
            } else {
                current_section = None;
            }
            continue;
        }

        match current_section {
            Some("environment") => {
                if let Some(key) = extract_yaml_key(trimmed)
                    && key == "flutter"
                {
                    is_flutter = true;
                }
            }
            Some("dependencies" | "dev_dependencies" | "dependency_overrides")
                if is_sdk_flutter_line(trimmed) =>
            {
                is_flutter = true;
            }
            _ => {}
        }
    }

    if is_flutter {
        ProjectKind::Flutter
    } else {
        ProjectKind::Dart
    }
}

/// Resolves the executable path for `flutter` or `dart`.
///
/// Priority:
/// 1. FVM local SDK (`<project_root>/.fvm/flutter_sdk/bin/<tool>`)
/// 2. User-configured SDK path (`custom_sdk_path/bin/<tool>`)
/// 3. Global executable on `PATH` (with `.bat` compatibility on Windows)
pub fn resolve_sdk_executable(
    project_root: &Path,
    kind: ProjectKind,
    custom_sdk_path: Option<&str>,
) -> String {
    let exe_name = platform_cli_filename(kind.cli_name());

    let fvm_candidate = project_root
        .join(".fvm")
        .join("flutter_sdk")
        .join("bin")
        .join(exe_name);
    if fvm_candidate.is_file() {
        return fvm_candidate.to_string_lossy().into_owned();
    }

    if let Some(sdk_dir) = custom_sdk_path
        && !sdk_dir.trim().is_empty()
    {
        let sdk_path = Path::new(sdk_dir.trim());
        let direct = sdk_path.join("bin").join(exe_name);
        if direct.is_file() {
            return direct.to_string_lossy().into_owned();
        }
        let nested = sdk_path.join(exe_name);
        if nested.is_file() {
            return nested.to_string_lossy().into_owned();
        }
    }

    exe_name.to_string()
}

/// Returns the platform-appropriate CLI filename (`flutter.bat` / `dart.bat` on Windows).
pub fn platform_cli_filename(base: &str) -> &'static str {
    #[cfg(windows)]
    {
        match base {
            "flutter" => "flutter.bat",
            "dart" => "dart.bat",
            _ => "flutter.bat",
        }
    }
    #[cfg(not(windows))]
    {
        match base {
            "flutter" => "flutter",
            "dart" => "dart",
            _ => "flutter",
        }
    }
}

fn strip_yaml_comment(line: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    for (idx, ch) in line.char_indices() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double => return &line[..idx],
            _ => {}
        }
    }
    line
}

fn extract_yaml_key(trimmed: &str) -> Option<&str> {
    if trimmed.starts_with('-') {
        return None;
    }
    let colon = trimmed.find(':')?;
    let key = trimmed[..colon].trim();
    if key.is_empty() { None } else { Some(key) }
}

fn is_sdk_flutter_line(trimmed: &str) -> bool {
    let Some((key, value)) = trimmed.split_once(':') else {
        return false;
    };
    if key.trim() != "sdk" {
        return false;
    }
    let val = value.trim().trim_matches(|c| c == '"' || c == '\'');
    val == "flutter"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_flutter_app_with_top_level_flutter_section() {
        let yaml = r#"
name: my_app
environment:
  sdk: ^3.7.0
flutter:
  uses-material-design: true
"#;
        assert_eq!(classify_pubspec(yaml), ProjectKind::Flutter);
    }

    #[test]
    fn classifies_flutter_plugin_with_sdk_flutter_dependency() {
        let yaml = r#"
name: my_plugin
dependencies:
  flutter:
    sdk: flutter
"#;
        assert_eq!(classify_pubspec(yaml), ProjectKind::Flutter);
    }

    #[test]
    fn classifies_flutter_by_environment_constraint() {
        let yaml = r#"
name: my_widget_kit
environment:
  sdk: ^3.7.0
  flutter: ">=3.24.0"
"#;
        assert_eq!(classify_pubspec(yaml), ProjectKind::Flutter);
    }

    #[test]
    fn classifies_pure_dart_package_correctly() {
        let yaml = r#"
name: pure_dart_pkg
# flutter: commented out
environment:
  sdk: ^3.7.0
dependencies:
  http: ^1.3.0
dev_dependencies:
  test: ^1.25.0
"#;
        assert_eq!(classify_pubspec(yaml), ProjectKind::Dart);
    }

    #[test]
    fn pubspec_overrides_resolves_sibling_pubspec() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("pubspec.yaml"),
            "name: app\ndependencies:\n  flutter:\n    sdk: flutter\n",
        )
        .expect("write pubspec");
        let overrides = dir.path().join("pubspec_overrides.yaml");
        fs::write(&overrides, "dependency_overrides:\n  foo: ^1.0.0\n").expect("write overrides");

        let info = find_project_for_path(&overrides).expect("project found");
        assert_eq!(info.kind, ProjectKind::Flutter);
        assert_eq!(info.root, dir.path());
    }
}
