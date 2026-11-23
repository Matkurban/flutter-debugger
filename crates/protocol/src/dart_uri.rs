use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct PackageConfigFile {
    #[serde(default)]
    packages: Vec<PackageConfigEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageConfigEntry {
    name: String,
    root_uri: String,
    #[serde(default)]
    package_uri: Option<String>,
}

/// Resolves a Dart `file:///...` or `package:<pkg>/<rel>.dart` URI to a local disk path.
/// For `package:` URIs, reads `<project_root>/.dart_tool/package_config.json` to map package roots,
/// with a fallback to `<project_root>/lib/<rel>.dart` if the package matches the workspace project.
pub fn resolve_dart_uri_to_local_path(uri: &str, project_root: Option<&Path>) -> Option<PathBuf> {
    let trimmed = uri.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("file://") {
        if let Ok(parsed) = url::Url::parse(trimmed)
            && let Ok(path) = parsed.to_file_path()
        {
            return Some(clean_relative_segments(&path));
        }
        let stripped = trimmed.strip_prefix("file://")?;
        return Some(clean_relative_segments(Path::new(stripped)));
    }

    if let Some(pkg_spec) = trimmed.strip_prefix("package:") {
        let (pkg_name, rel_path) = pkg_spec.split_once('/')?;
        let root = project_root?;
        let config_dir = root.join(".dart_tool");
        let config_path = config_dir.join("package_config.json");
        if let Ok(bytes) = fs::read(&config_path)
            && let Ok(parsed) = serde_json::from_slice::<PackageConfigFile>(&bytes)
            && let Some(entry) = parsed.packages.iter().find(|p| p.name == pkg_name)
            && let Some(pkg_root) = resolve_package_root_uri(&entry.root_uri, &config_dir)
        {
            let package_sub = entry
                .package_uri
                .as_deref()
                .unwrap_or("lib/")
                .trim_end_matches('/');

            let full_path = if package_sub.is_empty() || package_sub == "." {
                pkg_root.join(rel_path)
            } else {
                pkg_root.join(package_sub).join(rel_path)
            };

            return Some(clean_relative_segments(&full_path));
        }

        // Fallback if package_config.json is absent or doesn't list the local workspace package
        let candidate_local = clean_relative_segments(&root.join("lib").join(rel_path));
        let root_name_matches = root
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == pkg_name);
        if candidate_local.exists() || root_name_matches || pubspec_name_matches(root, pkg_name) {
            return Some(candidate_local);
        }
        return None;
    }

    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        return Some(clean_relative_segments(candidate));
    }
    project_root.map(|r| clean_relative_segments(&r.join(candidate)))
}

fn pubspec_name_matches(project_root: &Path, pkg_name: &str) -> bool {
    let Ok(content) = fs::read_to_string(project_root.join("pubspec.yaml")) else {
        return false;
    };
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("name:") {
            return rest.trim().trim_matches(|c| c == '"' || c == '\'') == pkg_name;
        }
    }
    false
}

fn resolve_package_root_uri(root_uri: &str, config_dir: &Path) -> Option<PathBuf> {
    if root_uri.starts_with("file://") {
        if let Ok(parsed) = url::Url::parse(root_uri)
            && let Ok(path) = parsed.to_file_path()
        {
            return Some(clean_relative_segments(&path));
        }
        return root_uri
            .strip_prefix("file://")
            .map(|s| clean_relative_segments(Path::new(s)));
    }
    Some(clean_relative_segments(&config_dir.join(root_uri)))
}

pub fn clean_relative_segments(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Rewrites `file:///.../*.dart(:line(:col)?)?` and `package:<pkg>/.../*.dart(:line(:col)?)?`
/// inside `text` into Zed-clickable local paths:
/// - Workspace-relative path (`lib/pages/home.dart:42:12`) when inside `project_root`
/// - Clean absolute path (`/Users/.../provider.dart:19:7`) when outside `project_root`
/// - Converts glued Flutter widget error prefixes like `Scaffold:file:///.../home.dart:88:12`
///   into `Scaffold (lib/home.dart:88:12)` so the path has clear token boundaries.
///
/// Also returns the first resolved `(abs_path, line, Option<column>)` for enriching DAP `output` events.
pub fn rewrite_dart_output_paths(
    text: &str,
    project_root: &Path,
) -> (String, Option<(PathBuf, u32, Option<u32>)>) {
    if !text.contains("file://") && !text.contains("package:") {
        return (text.to_string(), None);
    }

    let root_clean = clean_relative_segments(project_root);
    let root_canon = fs::canonicalize(project_root).ok();

    let mut out = String::with_capacity(text.len());
    let mut first_resolved: Option<(PathBuf, u32, Option<u32>)> = None;
    let mut cursor = 0usize;

    while cursor < text.len() {
        let rest = &text[cursor..];
        let next_file = rest.find("file://");
        let next_pkg = rest.find("package:");
        let Some(rel_match) = (match (next_file, next_pkg) {
            (Some(f), Some(p)) => Some(f.min(p)),
            (Some(f), None) => Some(f),
            (None, Some(p)) => Some(p),
            (None, None) => None,
        }) else {
            out.push_str(rest);
            break;
        };

        let match_start = cursor + rel_match;
        let candidate = &text[match_start..];
        let token_end_rel = candidate
            .find(|c: char| {
                c.is_whitespace()
                    || c.is_control()
                    || matches!(c, ')' | ']' | '}' | '>' | '\'' | '"' | ',' | ';')
            })
            .unwrap_or(candidate.len());
        let raw_token = &candidate[..token_end_rel];

        let Some(dart_idx) = raw_token.rfind(".dart") else {
            // Not a `.dart` URI; advance past the scheme prefix.
            let advance = if candidate.starts_with("file://") {
                "file://".len()
            } else {
                "package:".len()
            };
            out.push_str(&text[cursor..match_start + advance]);
            cursor = match_start + advance;
            continue;
        };

        let uri_len = dart_idx + ".dart".len();
        let uri_part = &raw_token[..uri_len];
        let after_uri = &candidate[uri_len..];
        let (consumed_tail_len, line_opt, col_opt) = parse_line_col_suffix(after_uri);
        let full_match_end = match_start + uri_len + consumed_tail_len;

        let Some(resolved_clean) = resolve_dart_uri_to_local_path(uri_part, Some(&root_clean))
        else {
            out.push_str(&text[cursor..full_match_end]);
            cursor = full_match_end;
            continue;
        };

        let resolved_canon = fs::canonicalize(&resolved_clean).ok();
        let rel_inside_project = resolved_clean
            .strip_prefix(&root_clean)
            .ok()
            .map(Path::to_path_buf)
            .or_else(|| {
                if let (Some(rc), Some(pc)) = (&resolved_canon, &root_canon) {
                    rc.strip_prefix(pc).ok().map(Path::to_path_buf)
                } else {
                    None
                }
            });

        let abs_path = resolved_canon.unwrap_or_else(|| resolved_clean.clone());
        let display_path = match rel_inside_project {
            Some(rel) => normalize_slashes(&rel),
            None => normalize_slashes(&abs_path),
        };

        let formatted_target = match (line_opt, col_opt) {
            (Some(l), Some(c)) => format!("{display_path}:{l}:{c}"),
            (Some(l), None) => format!("{display_path}:{l}"),
            (None, _) => display_path,
        };

        if first_resolved.is_none() {
            first_resolved = Some((abs_path, line_opt.unwrap_or(1), col_opt));
        }

        // Check if this URI is glued to a widget/class name with a colon, e.g. `Scaffold:file:///...`
        let prefix_slice = &text[cursor..match_start];
        if has_glued_widget_colon_prefix(prefix_slice) {
            out.push_str(&prefix_slice[..prefix_slice.len() - 1]);
            out.push_str(" (");
            out.push_str(&formatted_target);
            out.push(')');
        } else {
            out.push_str(prefix_slice);
            out.push_str(&formatted_target);
        }

        cursor = full_match_end;
    }

    (out, first_resolved)
}

fn has_glued_widget_colon_prefix(prefix: &str) -> bool {
    let Some(before_colon) = prefix.strip_suffix(':') else {
        return false;
    };
    let Some(prev_char) = before_colon.chars().next_back() else {
        return false;
    };
    prev_char.is_ascii_alphanumeric() || prev_char == '_' || prev_char == '>'
}

fn parse_line_col_suffix(after_uri: &str) -> (usize, Option<u32>, Option<u32>) {
    let bytes = after_uri.as_bytes();
    if bytes.first() != Some(&b':') {
        return (0, None, None);
    }
    let line_digits_len = bytes[1..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if line_digits_len == 0 {
        return (0, None, None);
    }
    let Ok(line) = after_uri[1..1 + line_digits_len].parse::<u32>() else {
        return (0, None, None);
    };
    let mut consumed = 1 + line_digits_len;

    if bytes.get(consumed) == Some(&b':') {
        let col_digits_len = bytes[consumed + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if col_digits_len > 0
            && let Ok(col) =
                after_uri[consumed + 1..consumed + 1 + col_digits_len].parse::<u32>()
        {
            consumed += 1 + col_digits_len;
            return (consumed, Some(line), Some(col));
        }
    }

    (consumed, Some(line), None)
}

fn normalize_slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup_mock_project() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let project_root = dir.path().join("my_app");
        let dart_tool = project_root.join(".dart_tool");
        fs::create_dir_all(&dart_tool).expect("mkdir .dart_tool");

        let external_pkg = dir.path().join("pub_cache").join("provider-6.1.0");
        fs::create_dir_all(external_pkg.join("lib").join("src")).expect("mkdir provider");

        let pkg_config = json!({
            "configVersion": 2,
            "packages": [
                {
                    "name": "my_app",
                    "rootUri": "../",
                    "packageUri": "lib/"
                },
                {
                    "name": "provider",
                    "rootUri": format!("file://{}", external_pkg.display()),
                    "packageUri": "lib/"
                }
            ]
        });
        fs::write(
            dart_tool.join("package_config.json"),
            serde_json::to_vec_pretty(&pkg_config).expect("json"),
        )
        .expect("write package_config.json");

        (dir, project_root, external_pkg)
    }

    #[test]
    fn rewrites_glued_flutter_error_file_uri_to_workspace_relative() {
        let root = Path::new("/Users/dev/app");
        let input = "The relevant error-causing widget was:\n  Scaffold:file:///Users/dev/app/lib/home.dart:88:12\n";
        let (rewritten, first) = rewrite_dart_output_paths(input, root);
        assert_eq!(
            rewritten,
            "The relevant error-causing widget was:\n  Scaffold (lib/home.dart:88:12)\n"
        );
        let (path, line, col) = first.expect("resolved first location");
        assert_eq!(path, PathBuf::from("/Users/dev/app/lib/home.dart"));
        assert_eq!(line, 88);
        assert_eq!(col, Some(12));
    }

    #[test]
    fn rewrites_package_stack_frames_with_and_without_ansi_escapes() {
        let (_dir, project_root, _ext_pkg) = setup_mock_project();

        let plain = "#0      _HomeState.build (package:my_app/pages/home.dart:42:5)\n";
        let (rewritten_plain, first_plain) = rewrite_dart_output_paths(plain, &project_root);
        assert_eq!(
            rewritten_plain,
            "#0      _HomeState.build (lib/pages/home.dart:42:5)\n"
        );
        let (p1, l1, c1) = first_plain.expect("plain loc");
        assert_eq!(p1, project_root.join("lib/pages/home.dart"));
        assert_eq!(l1, 42);
        assert_eq!(c1, Some(5));

        let ansi = "\x1b[2m#0      _HomeState.build (package:my_app/pages/home.dart:42:5)\x1b[0m\n";
        let (rewritten_ansi, first_ansi) = rewrite_dart_output_paths(ansi, &project_root);
        assert_eq!(
            rewritten_ansi,
            "\x1b[2m#0      _HomeState.build (lib/pages/home.dart:42:5)\x1b[0m\n"
        );
        let (p2, l2, c2) = first_ansi.expect("ansi loc");
        assert_eq!(p2, project_root.join("lib/pages/home.dart"));
        assert_eq!(l2, 42);
        assert_eq!(c2, Some(5));
    }

    #[test]
    fn rewrites_external_pub_package_to_clean_absolute_path_and_preserves_http_urls() {
        let (_dir, project_root, external_pkg) = setup_mock_project();
        let expected_provider = external_pkg.join("lib/src/provider.dart");

        let input = "Docs at https://flutter.dev/docs and http://127.0.0.1:9100\n#1 Provider.of (package:provider/src/provider.dart:19:7)";
        let (rewritten, first) = rewrite_dart_output_paths(input, &project_root);

        assert!(rewritten.contains("https://flutter.dev/docs"));
        assert!(rewritten.contains("http://127.0.0.1:9100"));
        assert!(
            rewritten.contains(&format!(
                "#1 Provider.of ({}:19:7)",
                expected_provider.display()
            )),
            "Actual rewritten: {rewritten}"
        );
        assert!(!rewritten.contains("package:provider"));
        assert!(!rewritten.contains("file://"));

        let (path, line, col) = first.expect("provider loc");
        assert_eq!(path, expected_provider);
        assert_eq!(line, 19);
        assert_eq!(col, Some(7));
    }
}
