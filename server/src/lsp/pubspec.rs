use serde_json::json;
use tower_lsp_server::ls_types::{
    CodeLens, Command, DocumentLink, InlayHint, InlayHintKind, InlayHintLabel, Position, Range, Uri,
};

use crate::cache::PubVersionCache;
use crate::project::{ProjectInfo, ProjectKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubDependency {
    pub name: String,
    pub line: u32,
    pub name_start_col: u32,
    pub name_end_col: u32,
    pub line_end_col: u32,
    pub version_constraint: Option<String>,
    pub is_hosted_pub_dev: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedPubspec {
    pub dependencies: Vec<PubDependency>,
}

/// Synchronously parses `pubspec.yaml` or `pubspec_overrides.yaml` dependencies.
pub fn parse_pubspec_dependencies(content: &str) -> ParsedPubspec {
    let mut deps: Vec<PubDependency> = Vec::new();
    let mut in_dep_section = false;
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
            let key = trimmed.split(':').next().map(str::trim).unwrap_or_default();
            in_dep_section = matches!(
                key,
                "dependencies" | "dev_dependencies" | "dependency_overrides"
            );
            continue;
        }

        if !in_dep_section {
            continue;
        }

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
                "flutter" | "flutter_test" | "flutter_localizations" | "flutter_web_plugins" | "flutter_driver"
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
                is_hosted_pub_dev: is_hosted,
            });

            if val.is_empty() {
                pending_block_dep_index = Some(deps.len() - 1);
            }
        } else if indent > 2
            && let Some(idx) = pending_block_dep_index
        {
            if let Some((sub_key, sub_val)) = trimmed.split_once(':') {
                let sk = sub_key.trim();
                let sv = sub_val.trim().trim_matches(|c| c == '"' || c == '\'');
                if matches!(sk, "sdk" | "path" | "git") {
                    deps[idx].is_hosted_pub_dev = false;
                } else if sk == "version" && !sv.is_empty() {
                    deps[idx].version_constraint = Some(sv.to_string());
                }
            }
        }
    }

    ParsedPubspec { dependencies: deps }
}

/// Builds side-effect-free `textDocument/documentLink` targets pointing to `pub.dev`.
pub fn build_document_links(parsed: &ParsedPubspec) -> Vec<DocumentLink> {
    parsed
        .dependencies
        .iter()
        .filter(|dep| dep.is_hosted_pub_dev)
        .filter_map(|dep| {
            let target_str = format!("https://pub.dev/packages/{}", dep.name);
            let target = target_str.parse::<Uri>().ok()?;
            Some(DocumentLink {
                range: Range {
                    start: Position::new(dep.line, dep.name_start_col),
                    end: Position::new(dep.line, dep.name_end_col),
                },
                target: Some(target),
                tooltip: Some(format!("Open {} on pub.dev", dep.name)),
                data: None,
            })
        })
        .collect()
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
                label: InlayHintLabel::String(format!("  ↑ {}", cached.latest_version)),
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

/// Builds top-of-file monochrome Unicode CodeLens actions for `pubspec.yaml` / `pubspec_overrides.yaml`.
pub fn build_pubspec_code_lenses(
    uri: &Uri,
    project: &ProjectInfo,
    selected_device_label: Option<&str>,
) -> Vec<CodeLens> {
    let top_range = Range {
        start: Position::new(0, 0),
        end: Position::new(0, 0),
    };
    let uri_str = uri.as_str().to_string();

    let mut lenses = vec![
        make_lens(
            top_range,
            "↓ Pub Get",
            "flutterDebugger.pubGet",
            vec![json!(uri_str)],
        ),
        make_lens(
            top_range,
            "↑ Pub Upgrade",
            "flutterDebugger.pubUpgrade",
            vec![json!(uri_str)],
        ),
        make_lens(
            top_range,
            "≡ Pub Outdated",
            "flutterDebugger.pubOutdated",
            vec![json!(uri_str)],
        ),
    ];

    if project.has_build_runner {
        lenses.push(make_lens(
            top_range,
            "⟳ Build Runner",
            "flutterDebugger.buildRunner",
            vec![json!(uri_str)],
        ));
    }

    if project.kind == ProjectKind::Flutter {
        let device_title = match selected_device_label {
            Some(label) => format!("◎ Device: {label}"),
            None => "◎ Device: Auto".to_string(),
        };
        lenses.push(make_lens(
            top_range,
            &device_title,
            "flutterDebugger.cycleDevice",
            vec![json!(uri_str)],
        ));
        lenses.push(make_lens(
            top_range,
            "↻ Refresh Devices",
            "flutterDebugger.refreshDevices",
            vec![json!(uri_str)],
        ));
        lenses.push(make_lens(
            top_range,
            "≡ Doctor",
            "flutterDebugger.doctor",
            vec![json!(uri_str)],
        ));
    }

    lenses.push(make_lens(
        top_range,
        "↻ Refresh Versions",
        "flutterDebugger.refreshPubVersions",
        vec![json!(uri_str)],
    ));

    lenses.push(make_lens(
        top_range,
        "⊞ New Project",
        "flutterDebugger.openProjectWizard",
        vec![json!(uri_str)],
    ));

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

    #[test]
    fn parses_hosted_and_skips_sdk_path_git_dependencies() {
        let yaml = r#"
name: demo
dependencies:
  flutter:
    sdk: flutter
  http: ^1.2.0
  local_pkg:
    path: ../local_pkg
  git_pkg:
    git:
      url: https://example.com/repo.git
dev_dependencies:
  build_runner: ^2.4.10
"#;
        let parsed = parse_pubspec_dependencies(yaml);
        let hosted_names: Vec<&str> = parsed
            .dependencies
            .iter()
            .filter(|d| d.is_hosted_pub_dev)
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(hosted_names, vec!["http", "build_runner"]);

        let links = build_document_links(&parsed);
        assert_eq!(links.len(), 2);
        assert_eq!(
            links[0].target.as_ref().map(|u| u.as_str()),
            Some("https://pub.dev/packages/http")
        );
    }
}
