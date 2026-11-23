use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

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

type PackageConfigCache = HashMap<PathBuf, (Option<SystemTime>, Arc<PackageConfigFile>)>;

static PACKAGE_CONFIG_CACHE: OnceLock<Mutex<PackageConfigCache>> = OnceLock::new();

fn load_package_config(config_path: &Path) -> Option<Arc<PackageConfigFile>> {
    let mtime = fs::metadata(config_path).ok()?.modified().ok();
    let cache = PACKAGE_CONFIG_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock()
        && let Some((cached_mtime, cached_cfg)) = guard.get(config_path)
        && *cached_mtime == mtime
    {
        return Some(Arc::clone(cached_cfg));
    }

    let bytes = fs::read(config_path).ok()?;
    let parsed = Arc::new(serde_json::from_slice::<PackageConfigFile>(&bytes).ok()?);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(config_path.to_path_buf(), (mtime, Arc::clone(&parsed)));
    }
    Some(parsed)
}

/// Formats an absolute local file path and optional line/column into a Zed-clickable
/// `zed://file/<percent-encoded-abs-path>[:line[:col]]` URL.
///
/// - Normalizes path separators to `/` and ensures a single leading `/`
///   (e.g. `/Users/...` on Unix and `/C:/Users/...` on Windows).
/// - Percent-encodes all bytes outside RFC 3986 unreserved characters (`A-Z`, `a-z`, `0-9`,
///   `-`, `_`, `.`, `~`), `/`, and a Windows drive colon at index 2 (`/C:/...`).
/// - Appends unencoded `:line` or `:line:col` suffix so Zed's `PathWithPosition::parse_str`
///   can jump to the exact line and column.
pub fn format_zed_file_uri(abs_path: &Path, line: Option<u32>, col: Option<u32>) -> String {
    let normalized = normalize_slashes(abs_path);
    let trimmed = normalized.trim_start_matches('/');
    let with_leading_slash = format!("/{trimmed}");
    let bytes = with_leading_slash.as_bytes();

    let has_win_drive_colon = bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && (bytes.len() == 3 || bytes[3] == b'/');

    let mut encoded_path = String::with_capacity(bytes.len() + 16);
    for (i, &b) in bytes.iter().enumerate() {
        let keep_unencoded = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/')
            || (i == 2 && has_win_drive_colon);
        if keep_unencoded {
            encoded_path.push(b as char);
        } else {
            let _ = write!(encoded_path, "%{b:02X}");
        }
    }

    match (line, col) {
        (Some(l), Some(c)) => format!("zed://file{encoded_path}:{l}:{c}"),
        (Some(l), None) => format!("zed://file{encoded_path}:{l}"),
        (None, _) => format!("zed://file{encoded_path}"),
    }
}

/// Resolves a Dart `file:///...`, `package:<pkg>/<rel>.dart`, `zed://file/...`, or local path
/// to a normalized local disk path.
/// For `package:` URIs, reads `<project_root>/.dart_tool/package_config.json` to map package roots,
/// with a fallback to `<project_root>/lib/<rel>.dart` if the package matches the workspace project.
pub fn resolve_dart_uri_to_local_path(uri: &str, project_root: Option<&Path>) -> Option<PathBuf> {
    let trimmed = uri.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(stripped) = trimmed.strip_prefix("zed://file") {
        if stripped.is_empty() {
            return None;
        }
        let path_part = match stripped.rfind(".dart") {
            Some(idx) => {
                let end = idx + ".dart".len();
                let (consumed, line_opt, _) = parse_line_col_suffix(&stripped[end..]);
                if line_opt.is_some() && end + consumed == stripped.len() {
                    &stripped[..end]
                } else {
                    stripped
                }
            }
            None => stripped,
        };
        let decoded = percent_decode_str(path_part);
        let normalized = strip_windows_leading_slash(&decoded);
        return Some(clean_relative_segments(Path::new(normalized)));
    }

    if trimmed.starts_with("file://") {
        if let Ok(parsed) = url::Url::parse(trimmed)
            && let Ok(path) = parsed.to_file_path()
        {
            let lossy = path.to_string_lossy();
            let normalized = strip_windows_leading_slash(&lossy);
            return Some(clean_relative_segments(Path::new(normalized)));
        }
        let stripped = trimmed.strip_prefix("file://")?;
        let decoded = percent_decode_str(stripped);
        let normalized = strip_windows_leading_slash(&decoded);
        return Some(clean_relative_segments(Path::new(normalized)));
    }

    if let Some(pkg_spec) = trimmed.strip_prefix("package:") {
        let (pkg_name, rel_path) = pkg_spec.split_once('/')?;
        let root = project_root?;
        let config_dir = root.join(".dart_tool");
        let config_path = config_dir.join("package_config.json");
        if let Some(parsed) = load_package_config(&config_path)
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

    if is_windows_drive_path(trimmed) {
        let normalized = trimmed.replace('\\', "/");
        return Some(clean_relative_segments(Path::new(&normalized)));
    }

    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        return Some(clean_relative_segments(candidate));
    }
    project_root.map(|r| clean_relative_segments(&r.join(candidate)))
}

fn percent_decode_str(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            out.push(((hi << 4) | lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn strip_windows_leading_slash(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && (bytes.len() == 3 || bytes[3] == b'/' || bytes[3] == b'\\')
    {
        &s[1..]
    } else {
        s
    }
}

fn is_windows_drive_path(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
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
            let lossy = path.to_string_lossy();
            let normalized = strip_windows_leading_slash(&lossy);
            return Some(clean_relative_segments(Path::new(normalized)));
        }
        return root_uri.strip_prefix("file://").map(|s| {
            let decoded = percent_decode_str(s);
            let normalized = strip_windows_leading_slash(&decoded);
            clean_relative_segments(Path::new(normalized))
        });
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

/// Rewrites Dart source locations inside `text` into Zed-clickable `zed://file/...` URLs:
/// 1. `file:///.../*.dart(:line(:col)?)?` (including glued widget prefixes like `Swiper:file:///...`)
/// 2. `package:<pkg>/.../*.dart(:line(:col)?)?`
/// 3. Bare local `.dart:line(:col)?` paths when `:line` is present and the resolved file exists on disk.
///
/// Also returns the first resolved `(abs_path, line, Option<column>)` for enriching DAP `output` events.
pub fn rewrite_dart_output_paths(
    text: &str,
    project_root: &Path,
) -> (String, Option<(PathBuf, u32, Option<u32>)>) {
    if !text.contains(".dart") {
        return (text.to_string(), None);
    }

    let root_clean = clean_relative_segments(project_root);

    let mut out = String::with_capacity(text.len() + 32);
    let mut first_resolved: Option<(PathBuf, u32, Option<u32>)> = None;
    let mut emit_cursor = 0usize;
    let mut search_cursor = 0usize;

    while search_cursor < text.len() {
        let Some(rel_dart) = text[search_cursor..].find(".dart") else {
            break;
        };
        let dart_pos = search_cursor + rel_dart;
        let dart_end = dart_pos + ".dart".len();

        // `.dart_tool` boundary guard: `.dart` must not be followed by an identifier character.
        if text[dart_end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            search_cursor = dart_end;
            continue;
        }

        let (consumed_tail_len, line_opt, col_opt) = parse_line_col_suffix(&text[dart_end..]);
        let full_match_end = dart_end + consumed_tail_len;

        // Compute the start of the current line/control-free segment after `emit_cursor`.
        let segment_start = find_clean_segment_start(text, emit_cursor, dart_pos);
        let delim_token_start = find_delim_token_start(text, segment_start, dart_pos);
        let raw_candidate = &text[delim_token_start..dart_end];

        // Find where the URI/path candidate begins and what kind it is.
        let Some((match_start, is_bare_path)) =
            locate_dart_candidate(text, segment_start, delim_token_start, dart_pos, raw_candidate)
        else {
            search_cursor = full_match_end;
            continue;
        };

        // Bare local paths require `:line` to avoid rewriting casual prose mentions of `foo.dart`.
        if is_bare_path && line_opt.is_none() {
            search_cursor = full_match_end;
            continue;
        }

        let uri_part = &text[match_start..dart_end];
        let Some(resolved_clean) = resolve_dart_uri_to_local_path(uri_part, Some(&root_clean))
        else {
            search_cursor = full_match_end;
            continue;
        };

        // Bare local paths must correspond to an existing file on disk.
        if is_bare_path && !resolved_clean.is_file() {
            search_cursor = full_match_end;
            continue;
        }

        let formatted_target = format_zed_file_uri(&resolved_clean, line_opt, col_opt);

        if first_resolved.is_none() {
            first_resolved = Some((resolved_clean, line_opt.unwrap_or(1), col_opt));
        }

        let prefix_slice = &text[emit_cursor..match_start];
        if has_glued_widget_colon_prefix(prefix_slice) {
            out.push_str(&prefix_slice[..prefix_slice.len() - 1]);
            out.push_str(" (");
            out.push_str(&formatted_target);
            out.push(')');
        } else {
            out.push_str(prefix_slice);
            out.push_str(&formatted_target);
        }

        emit_cursor = full_match_end;
        search_cursor = full_match_end;
    }

    out.push_str(&text[emit_cursor..]);
    (out, first_resolved)
}

fn find_clean_segment_start(text: &str, emit_cursor: usize, dart_pos: usize) -> usize {
    let slice = &text[emit_cursor..dart_pos];
    let Some(rel_ctrl) = slice.rfind(|c: char| c.is_control()) else {
        return emit_cursor;
    };
    let ctrl_pos = emit_cursor + rel_ctrl;
    let bytes = text.as_bytes();
    if bytes.get(ctrl_pos) == Some(&0x1b) && bytes.get(ctrl_pos + 1) == Some(&b'[') {
        let mut idx = ctrl_pos + 2;
        while idx < dart_pos && (0x20..=0x3f).contains(&bytes[idx]) {
            idx += 1;
        }
        if idx < dart_pos && (0x40..=0x7e).contains(&bytes[idx]) {
            return idx + 1;
        }
    }
    ctrl_pos + 1
}

fn find_delim_token_start(text: &str, segment_start: usize, dart_pos: usize) -> usize {
    let slice = &text[segment_start..dart_pos];
    match slice.rfind(|c: char| {
        c.is_whitespace()
            || c.is_control()
            || matches!(
                c,
                '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '\'' | '"' | ',' | ';'
            )
    }) {
        Some(rel_idx) => {
            let ch_len = slice[rel_idx..]
                .chars()
                .next()
                .map_or(1, char::len_utf8);
            segment_start + rel_idx + ch_len
        }
        None => segment_start,
    }
}

fn locate_dart_candidate(
    text: &str,
    segment_start: usize,
    delim_token_start: usize,
    dart_pos: usize,
    raw_candidate: &str,
) -> Option<(usize, bool)> {
    // 1. Check for `file://` in `raw_candidate` or earlier in the current line segment (supporting spaces/parens in `file://` paths).
    if let Some(rel_file) = raw_candidate.find("file://") {
        return Some((delim_token_start + rel_file, false));
    }
    let line_segment = &text[segment_start..dart_pos];
    if let Some(rel_file) = line_segment.rfind("file://") {
        let file_start = segment_start + rel_file;
        let after_scheme = &text[file_start + "file://".len()..dart_pos];
        if !after_scheme.contains('\t')
            && !after_scheme.contains("http://")
            && !after_scheme.contains("https://")
            && !after_scheme.contains("ws://")
            && !after_scheme.contains("wss://")
            && !after_scheme.contains("zed://")
            && !after_scheme.contains("package:")
        {
            return Some((file_start, false));
        }
    }

    // 2. Check for `package:` (either direct or glued after `Widget:package:...`).
    if raw_candidate.starts_with("package:") {
        return Some((delim_token_start, false));
    }
    if let Some(rel_pkg) = raw_candidate.find(":package:") {
        return Some((delim_token_start + rel_pkg + 1, false));
    }

    // 3. Reject non-file schemes and already-rewritten `zed://` links.
    if raw_candidate.contains("://")
        || raw_candidate.starts_with("zed://")
        || raw_candidate.starts_with("http://")
        || raw_candidate.starts_with("https://")
        || raw_candidate.starts_with("ws://")
        || raw_candidate.starts_with("wss://")
        || raw_candidate.starts_with("dart:")
    {
        return None;
    }

    // 4. Bare local path (handle optional glued `Widget:lib/foo.dart` prefix, excluding Windows drive `C:/`).
    if !is_windows_drive_path(raw_candidate)
        && let Some(colon_idx) = raw_candidate.find(':')
    {
        let prefix_with_colon = &raw_candidate[..=colon_idx];
        if has_glued_widget_colon_prefix(prefix_with_colon) {
            let after_colon = &raw_candidate[colon_idx + 1..];
            if after_colon.starts_with("dart:") || after_colon.contains("://") {
                return None;
            }
            return Some((delim_token_start + colon_idx + 1, true));
        }
        return None;
    }

    Some((delim_token_start, true))
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
    fn rewrites_glued_flutter_error_file_uri_to_zed_file_uri() {
        let root = Path::new("/Users/dev/app");
        let input = "The relevant error-causing widget was:\n  Swiper Swiper:file:///Users/dev/app/lib/pages/home/nicepay_view.dart:374:40\n";
        let (rewritten, first) = rewrite_dart_output_paths(input, root);
        assert_eq!(
            rewritten,
            "The relevant error-causing widget was:\n  Swiper Swiper (zed://file/Users/dev/app/lib/pages/home/nicepay_view.dart:374:40)\n"
        );
        let (path, line, col) = first.expect("resolved first location");
        assert_eq!(
            path,
            PathBuf::from("/Users/dev/app/lib/pages/home/nicepay_view.dart")
        );
        assert_eq!(line, 374);
        assert_eq!(col, Some(40));
    }

    #[test]
    fn rewrites_package_stack_frames_with_and_without_ansi_escapes() {
        let (_dir, project_root, _ext_pkg) = setup_mock_project();
        let expected_home = project_root.join("lib/pages/home.dart");
        let expected_uri = format_zed_file_uri(&expected_home, Some(42), Some(5));

        let plain = "#0      _HomeState.build (package:my_app/pages/home.dart:42:5)\n";
        let (rewritten_plain, first_plain) = rewrite_dart_output_paths(plain, &project_root);
        assert_eq!(
            rewritten_plain,
            format!("#0      _HomeState.build ({expected_uri})\n")
        );
        let (p1, l1, c1) = first_plain.expect("plain loc");
        assert_eq!(p1, expected_home);
        assert_eq!(l1, 42);
        assert_eq!(c1, Some(5));

        let ansi = "\x1b[2m#0      _HomeState.build (package:my_app/pages/home.dart:42:5)\x1b[0m\n";
        let (rewritten_ansi, first_ansi) = rewrite_dart_output_paths(ansi, &project_root);
        assert_eq!(
            rewritten_ansi,
            format!("\x1b[2m#0      _HomeState.build ({expected_uri})\x1b[0m\n")
        );
        let (p2, l2, c2) = first_ansi.expect("ansi loc");
        assert_eq!(p2, expected_home);
        assert_eq!(l2, 42);
        assert_eq!(c2, Some(5));
    }

    #[test]
    fn rewrites_external_pub_package_to_zed_file_uri_and_preserves_http_urls() {
        let (_dir, project_root, external_pkg) = setup_mock_project();
        let expected_provider = external_pkg.join("lib/src/provider.dart");
        let expected_uri = format_zed_file_uri(&expected_provider, Some(19), Some(7));

        let input = "Docs at https://flutter.dev/docs and http://127.0.0.1:9100\n#1 Provider.of (package:provider/src/provider.dart:19:7)";
        let (rewritten, first) = rewrite_dart_output_paths(input, &project_root);

        assert!(rewritten.contains("https://flutter.dev/docs"));
        assert!(rewritten.contains("http://127.0.0.1:9100"));
        assert!(
            rewritten.contains(&format!("#1 Provider.of ({expected_uri})")),
            "Actual rewritten: {rewritten}"
        );
        assert!(!rewritten.contains("package:provider"));
        assert!(!rewritten.contains("file://"));

        let (path, line, col) = first.expect("provider loc");
        assert_eq!(path, expected_provider);
        assert_eq!(line, 19);
        assert_eq!(col, Some(7));
    }

    #[test]
    fn rewrites_bare_relative_paths_when_file_exists_and_skips_missing_or_lineless_paths() {
        let (_dir, project_root, _ext_pkg) = setup_mock_project();
        let nicepay_file = project_root.join("lib/pages/home/nicepay_view.dart");
        fs::create_dir_all(nicepay_file.parent().expect("parent")).expect("mkdir pages/home");
        fs::write(&nicepay_file, "void main() {}\n").expect("write nicepay_view.dart");

        let expected_uri = format_zed_file_uri(&nicepay_file, Some(374), Some(40));

        // 1. Bare relative path inside parentheses
        let widget_line = "  Swiper Swiper (lib/pages/home/nicepay_view.dart:374:40)\n";
        let (rewritten_widget, first_widget) =
            rewrite_dart_output_paths(widget_line, &project_root);
        assert_eq!(rewritten_widget, format!("  Swiper Swiper ({expected_uri})\n"));
        assert_eq!(
            first_widget,
            Some((nicepay_file.clone(), 374, Some(40)))
        );

        // 2. Compiler error prefix `lib/...:374:40: Error: ...` and glued `Swiper:lib/...:374:40`
        let compiler_line = "lib/pages/home/nicepay_view.dart:374:40: Error: Getter not found.\nSwiper:lib/pages/home/nicepay_view.dart:374:40\n";
        let (rewritten_compiler, _) = rewrite_dart_output_paths(compiler_line, &project_root);
        assert_eq!(
            rewritten_compiler,
            format!("{expected_uri}: Error: Getter not found.\nSwiper ({expected_uri})\n")
        );

        // 3. No `:line` or non-existent file must NOT be rewritten
        let untouched = "Edited lib/pages/home/nicepay_view.dart and lib/pages/home/missing.dart:12:4\n";
        let (rewritten_untouched, first_untouched) =
            rewrite_dart_output_paths(untouched, &project_root);
        assert_eq!(rewritten_untouched, untouched);
        assert!(first_untouched.is_none());
    }

    #[test]
    fn percent_encodes_spaces_parens_and_unicode_without_double_encoding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let special_root = dir.path().join("我的 项目 (1)");
        let dart_file = special_root.join("lib/main.dart");
        fs::create_dir_all(dart_file.parent().expect("parent")).expect("mkdir");
        fs::write(&dart_file, "void main() {}\n").expect("write");

        let uri = format_zed_file_uri(&dart_file, Some(10), Some(2));
        assert!(uri.starts_with("zed://file/"));
        assert!(uri.contains("%20"));
        assert!(uri.contains("%281%29"));
        assert!(uri.contains("%E6%88%91%E7%9A%84"));
        assert!(uri.ends_with("/lib/main.dart:10:2"));
        assert!(!uri.contains(' '));
        assert!(!uri.contains('('));
        assert!(!uri.contains(')'));

        // Already percent-encoded `file://` URI must not be double-encoded into `%2520`
        let pre_encoded_input = "at file:///Users/dev/my%20app%20%281%29/lib/main.dart:10:2\n";
        let (rewritten, first) = rewrite_dart_output_paths(pre_encoded_input, &special_root);
        assert_eq!(
            rewritten,
            "at zed://file/Users/dev/my%20app%20%281%29/lib/main.dart:10:2\n"
        );
        assert!(!rewritten.contains("%2520"));
        assert_eq!(
            first,
            Some((
                PathBuf::from("/Users/dev/my app (1)/lib/main.dart"),
                10,
                Some(2)
            ))
        );

        // `resolve_dart_uri_to_local_path` round-trips `zed://file/...`
        let resolved_back = resolve_dart_uri_to_local_path(&uri, None).expect("resolve zed://file");
        assert_eq!(resolved_back, clean_relative_segments(&dart_file));
    }

    #[test]
    fn preserves_dart_tool_boundary_idempotency_dart_scheme_and_windows_drive_paths() {
        let root = Path::new("/Users/dev/app");

        // 1. `.dart_tool` directory in path must not be truncated at `.dart`
        let dart_tool_input =
            "file:///Users/dev/app/.dart_tool/flutter_build/generated_main.dart:14:3\n";
        let (rewritten_dt, _) = rewrite_dart_output_paths(dart_tool_input, root);
        assert_eq!(
            rewritten_dt,
            "zed://file/Users/dev/app/.dart_tool/flutter_build/generated_main.dart:14:3\n"
        );

        // 2. Idempotency: already `zed://file/...` and `dart:ui/...` and `ws://` must stay unchanged
        let idempotent_input = "Already linked zed://file/Users/dev/app/lib/main.dart:10:5 and frame (dart:ui/painting.dart:88:1) on ws://127.0.0.1:8181/ws\n";
        let (rewritten_idem, first_idem) = rewrite_dart_output_paths(idempotent_input, root);
        assert_eq!(rewritten_idem, idempotent_input);
        assert!(first_idem.is_none());

        // 3. Windows drive path formatting and resolution
        let win_path = Path::new(r"C:\Users\dev\app\lib\main.dart");
        let win_uri = format_zed_file_uri(win_path, Some(25), Some(8));
        assert_eq!(win_uri, "zed://file/C:/Users/dev/app/lib/main.dart:25:8");
        assert_eq!(
            resolve_dart_uri_to_local_path(&win_uri, None),
            Some(PathBuf::from("C:/Users/dev/app/lib/main.dart"))
        );
        assert_eq!(
            resolve_dart_uri_to_local_path(r"C:\Users\dev\app\lib\main.dart", None),
            Some(PathBuf::from("C:/Users/dev/app/lib/main.dart"))
        );
    }

    #[test]
    fn caches_package_config_by_mtime_and_invalidates_on_file_change() {
        let (_dir, project_root, _ext_pkg) = setup_mock_project();
        let config_path = project_root.join(".dart_tool").join("package_config.json");

        let first = load_package_config(&config_path).expect("load 1");
        let second = load_package_config(&config_path).expect("load 2");
        assert!(Arc::ptr_eq(&first, &second));
    }
}
