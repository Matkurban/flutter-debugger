use std::path::Path;

use serde_json::json;
use tower_lsp_server::ls_types::{
    CodeLens, Command, Diagnostic, DiagnosticSeverity, DocumentLink, InlayHint, InlayHintKind,
    InlayHintLabel, Position, Range, Uri,
};

use crate::cache::PubVersionCache;
use crate::lsp::state::file_path_to_uri;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubDependency {
    pub name: String,
    pub line: u32,
    pub name_start_col: u32,
    pub name_end_col: u32,
    pub line_end_col: u32,
    pub version_constraint: Option<String>,
    pub local_path: Option<(String, u32, u32, u32)>,
    pub is_hosted_pub_dev: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlutterAssetEntry {
    pub path: String,
    pub line: u32,
    pub start_col: u32,
    pub end_col: u32,
    pub is_font: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedPubspec {
    pub dependencies: Vec<PubDependency>,
    pub assets: Vec<FlutterAssetEntry>,
}

/// Synchronously parses `pubspec.yaml` or `pubspec_overrides.yaml` dependencies,
/// local `path:` dependencies, and `flutter.assets` / `flutter.fonts` entries.
pub fn parse_pubspec_dependencies(content: &str) -> ParsedPubspec {
    let mut deps: Vec<PubDependency> = Vec::new();
    let mut assets: Vec<FlutterAssetEntry> = Vec::new();
    let mut current_top_section: Option<&str> = None;
    let mut flutter_subsection: Option<&str> = None;
    let mut pending_block_dep_index: Option<usize> = None;

    for (line_idx, raw_line) in content.lines().enumerate() {
        let stripped = strip_comment(raw_line);
        let trimmed = stripped.trim();
        if trimmed.is_empty() {
            continue;
        }

        let indent = raw_line.len() - raw_line.trim_start_matches(' ').len();

        if indent == 0 {
            pending_block_dep_index = None;
            flutter_subsection = None;
            let key = trimmed.split(':').next().map(str::trim).unwrap_or_default();
            current_top_section = Some(key);
            continue;
        }

        match current_top_section {
            Some("dependencies" | "dev_dependencies" | "dependency_overrides") => {
                if indent == 2 {
                    pending_block_dep_index = None;
                    let Some((raw_key, raw_val)) = trimmed.split_once(':') else {
                        continue;
                    };
                    let name = raw_key.trim();
                    if !is_valid_package_name(name) {
                        continue;
                    }

                    let val = raw_val.trim().trim_matches(|c| c == '"' || c == '\'');
                    let is_sdk_builtin = matches!(
                        name,
                        "flutter"
                            | "flutter_test"
                            | "flutter_localizations"
                            | "flutter_web_plugins"
                            | "flutter_driver"
                    );

                    let name_start_col = raw_line.find(name).unwrap_or(indent) as u32;
                    let name_end_col = name_start_col + name.len() as u32;
                    let line_end_col = raw_line.trim_end().len() as u32;

                    let is_inline_map = val.starts_with('{');
                    let version_constraint = if val.is_empty() || is_inline_map {
                        None
                    } else {
                        Some(val.to_string())
                    };

                    let is_hosted = !is_sdk_builtin
                        && !is_inline_map
                        && !val.contains("path:")
                        && !val.contains("git:")
                        && !val.contains("sdk:");

                    deps.push(PubDependency {
                        name: name.to_string(),
                        line: line_idx as u32,
                        name_start_col,
                        name_end_col,
                        line_end_col,
                        version_constraint,
                        local_path: None,
                        is_hosted_pub_dev: is_hosted,
                    });

                    if val.is_empty() {
                        pending_block_dep_index = Some(deps.len() - 1);
                    }
                } else if indent > 2
                    && let Some(idx) = pending_block_dep_index
                    && let Some((sub_key, sub_val)) = trimmed.split_once(':')
                {
                    let sk = sub_key.trim();
                    let sv = sub_val.trim().trim_matches(|c| c == '"' || c == '\'');
                    if matches!(sk, "sdk" | "path" | "git") {
                        deps[idx].is_hosted_pub_dev = false;
                        if sk == "path" && !sv.is_empty() {
                            let start_col = raw_line.find(sv).unwrap_or(indent) as u32;
                            let end_col = start_col + sv.len() as u32;
                            deps[idx].local_path =
                                Some((sv.to_string(), line_idx as u32, start_col, end_col));
                        }
                    } else if sk == "version" && !sv.is_empty() {
                        deps[idx].version_constraint = Some(sv.to_string());
                    }
                }
            }
            Some("flutter") => {
                if indent == 2 {
                    let key = trimmed.split(':').next().map(str::trim).unwrap_or_default();
                    flutter_subsection = match key {
                        "assets" => Some("assets"),
                        "fonts" => Some("fonts"),
                        _ => None,
                    };
                } else if flutter_subsection == Some("assets") {
                    if let Some(rest) = trimmed.strip_prefix('-') {
                        let candidate = rest.trim();
                        if !candidate.is_empty() && !candidate.starts_with("path:") {
                            let cleaned = candidate.trim_matches(|c| c == '"' || c == '\'');
                            if !cleaned.is_empty() {
                                let start_col = raw_line.find(cleaned).unwrap_or(indent) as u32;
                                assets.push(FlutterAssetEntry {
                                    path: cleaned.to_string(),
                                    line: line_idx as u32,
                                    start_col,
                                    end_col: start_col + cleaned.len() as u32,
                                    is_font: false,
                                });
                            }
                        } else if let Some((_, pval)) = candidate.split_once(':') {
                            let cleaned = pval.trim().trim_matches(|c| c == '"' || c == '\'');
                            if !cleaned.is_empty() {
                                let start_col = raw_line.find(cleaned).unwrap_or(indent) as u32;
                                assets.push(FlutterAssetEntry {
                                    path: cleaned.to_string(),
                                    line: line_idx as u32,
                                    start_col,
                                    end_col: start_col + cleaned.len() as u32,
                                    is_font: false,
                                });
                            }
                        }
                    }
                } else if flutter_subsection == Some("fonts") {
                    let clean_trimmed = trimmed.trim_start_matches('-').trim();
                    if let Some((k, v)) = clean_trimmed.split_once(':')
                        && k.trim() == "asset"
                    {
                        let cleaned = v.trim().trim_matches(|c| c == '"' || c == '\'');
                        if !cleaned.is_empty() {
                            let start_col = raw_line.find(cleaned).unwrap_or(indent) as u32;
                            assets.push(FlutterAssetEntry {
                                path: cleaned.to_string(),
                                line: line_idx as u32,
                                start_col,
                                end_col: start_col + cleaned.len() as u32,
                                is_font: true,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    ParsedPubspec {
        dependencies: deps,
        assets,
    }
}

/// Builds `textDocument/documentLink` targets:
/// - `pub.dev` links for hosted dependencies (when `include_pub_dev_links` is true)
/// - Local file/folder links for `flutter.assets` & `flutter.fonts`
/// - Local `pubspec.yaml` links for `path:` dependencies
pub fn build_document_links(
    parsed: &ParsedPubspec,
    project_root: &Path,
    include_pub_dev_links: bool,
) -> Vec<DocumentLink> {
    let mut links = Vec::new();

    for dep in &parsed.dependencies {
        if include_pub_dev_links && dep.is_hosted_pub_dev {
            let target_str = format!("https://pub.dev/packages/{}", dep.name);
            if let Ok(target) = target_str.parse::<Uri>() {
                links.push(DocumentLink {
                    range: Range {
                        start: Position::new(dep.line, dep.name_start_col),
                        end: Position::new(dep.line, dep.name_end_col),
                    },
                    target: Some(target),
                    tooltip: Some(format!("Open {} on pub.dev", dep.name)),
                    data: None,
                });
            }
        }

        if let Some((rel_path, line, start_col, end_col)) = &dep.local_path {
            let pkg_dir = project_root.join(rel_path);
            let target_path = if pkg_dir.join("pubspec.yaml").is_file() {
                pkg_dir.join("pubspec.yaml")
            } else {
                pkg_dir
            };
            if target_path.exists()
                && let Some(target_uri) = file_path_to_uri(&target_path)
            {
                links.push(DocumentLink {
                    range: Range {
                        start: Position::new(*line, *start_col),
                        end: Position::new(*line, *end_col),
                    },
                    target: Some(target_uri),
                    tooltip: Some(format!("Open local package `{}`", dep.name)),
                    data: None,
                });
            }
        }
    }

    for asset in &parsed.assets {
        let full_path = project_root.join(&asset.path);
        if full_path.exists()
            && let Some(target_uri) = file_path_to_uri(&full_path)
        {
            links.push(DocumentLink {
                range: Range {
                    start: Position::new(asset.line, asset.start_col),
                    end: Position::new(asset.line, asset.end_col),
                },
                target: Some(target_uri),
                tooltip: Some(format!("Open {}", asset.path)),
                data: None,
            });
        }
    }

    links
}

/// Validates `flutter.assets` and `flutter.fonts` paths on disk, returning Warning diagnostics
/// for missing files or directories.
pub fn build_pubspec_asset_diagnostics(
    parsed: &ParsedPubspec,
    project_root: &Path,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for asset in &parsed.assets {
        // Skip package asset references (`packages/<pkg>/...`)
        if asset.path.starts_with("packages/") {
            continue;
        }
        let full_path = project_root.join(&asset.path);
        if !full_path.exists() {
            let kind_label = if asset.is_font { "Font asset" } else { "Asset" };
            diags.push(Diagnostic {
                range: Range {
                    start: Position::new(asset.line, asset.start_col),
                    end: Position::new(asset.line, asset.end_col),
                },
                severity: Some(DiagnosticSeverity::WARNING),
                source: Some("flutter-debugger".to_string()),
                message: format!("{kind_label} path `{}` does not exist.", asset.path),
                ..Default::default()
            });
        }
    }
    diags
}

/// Builds monochrome Unicode Inlay Hints showing newer versions from cache.
pub fn build_inlay_hints(parsed: &ParsedPubspec, pub_cache: &PubVersionCache) -> Vec<InlayHint> {
    parsed
        .dependencies
        .iter()
        .filter(|dep| dep.is_hosted_pub_dev)
        .filter_map(|dep| {
            let cached = pub_cache.packages.get(&dep.name)?;
            let current = dep
                .version_constraint
                .as_deref()
                .unwrap_or_default()
                .trim_start_matches(['^', '>', '<', '=', ' ']);

            if current.is_empty() || current == cached.latest_version {
                return None;
            }

            Some(InlayHint {
                position: Position::new(dep.line, dep.line_end_col),
                label: InlayHintLabel::String(format!("  latest: {}", cached.latest_version)),
                kind: Some(InlayHintKind::TYPE),
                text_edits: None,
                tooltip: None,
                padding_left: Some(true),
                padding_right: Some(false),
                data: None,
            })
        })
        .collect()
}

/// Builds top-of-file dependency-only CodeLens actions for `pubspec.yaml` / `pubspec_overrides.yaml`.
pub fn build_pubspec_code_lenses(uri: &Uri) -> Vec<CodeLens> {
    let top_range = Range {
        start: Position::new(0, 0),
        end: Position::new(0, 0),
    };
    let uri_str = uri.as_str().to_string();

    vec![
        make_lens(
            top_range,
            "Pub Get",
            "flutterDebugger.pubGet",
            vec![json!(uri_str)],
        ),
        make_lens(
            top_range,
            "Pub Upgrade",
            "flutterDebugger.pubUpgrade",
            vec![json!(uri_str)],
        ),
        make_lens(
            top_range,
            "Pub Outdated",
            "flutterDebugger.pubOutdated",
            vec![json!(uri_str)],
        ),
    ]
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

fn strip_comment(line: &str) -> &str {
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

fn is_valid_package_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn validates_flutter_assets_and_links_local_path_deps() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("assets/images")).expect("mkdir assets");
        fs::create_dir_all(dir.path().join("packages/core")).expect("mkdir core");
        fs::write(dir.path().join("packages/core/pubspec.yaml"), "name: core\n").expect("write");

        let yaml = r#"
name: demo
dependencies:
  flutter:
    sdk: flutter
  http: ^1.2.0
  core:
    path: packages/core
flutter:
  assets:
    - assets/images/
    - assets/missing.png
  fonts:
    - family: Inter
      fonts:
        - asset: fonts/Inter-Regular.ttf
"#;
        let parsed = parse_pubspec_dependencies(yaml);
        assert_eq!(parsed.assets.len(), 3);

        let diags = build_pubspec_asset_diagnostics(&parsed, dir.path());
        assert_eq!(diags.len(), 2);
        assert!(diags[0].message.contains("assets/missing.png"));
        assert!(diags[1].message.contains("fonts/Inter-Regular.ttf"));

        let links = build_document_links(&parsed, dir.path(), false);
        assert_eq!(links.len(), 2); // local `core` pubspec.yaml + `assets/images/`
    }
}
