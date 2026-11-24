use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde_json::json;
use tower_lsp_server::ls_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeLens, Command, CompletionItem,
    CompletionItemKind, CompletionTextEdit, Diagnostic, DiagnosticSeverity, DiagnosticTag,
    DocumentLink, Documentation, InlayHint, InlayHintLabel, InlayHintTooltip, MarkupContent,
    MarkupKind, NumberOrString, Position, Range, TextEdit, Uri, WorkspaceEdit,
};

use crate::lsp::pubspec::eval::DependencyEvaluation;
use crate::lsp::pubspec::parser::{
    DependencySource, FlutterAssetEntry, ParsedDependency, ParsedPubspec, VersionCompletionContext,
    parse_pubspec,
};
use crate::lsp::pubspec::registry::{ChangelogEntry, PackageInfo, select_relevant_entries};
use crate::lsp::pubspec::version::{
    UpgradeKind, VersionConstraint, compare_dart_versions, format_upgraded_constraint,
    is_sdk_compatible, parse_dart_version,
};
use crate::lsp::state::{PubspecConfig, file_path_to_uri};
use crate::project::ProjectKind;

pub const CMD_PUB_REFRESH: &str = "flutterDebugger.refreshPubVersions";
pub const CMD_PUB_GET: &str = "flutterDebugger.pubGet";
pub const CMD_PUB_UPGRADE: &str = "flutterDebugger.pubUpgrade";
pub const CMD_PUB_OUTDATED: &str = "flutterDebugger.pubOutdated";

/// Builds the unified list of diagnostics for `pubspec.yaml` / `pubspec_overrides.yaml`,
/// combining missing `flutter.assets`/`fonts` warnings with dependency version/health diagnostics
/// so they are always published atomically without overwriting each other.
#[must_use]
pub fn build_pubspec_diagnostics(
    parsed: &ParsedPubspec,
    evals: &[DependencyEvaluation],
    project_root: Option<&Path>,
    config: &PubspecConfig,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    if let Some(root) = project_root {
        for asset in &parsed.assets {
            if asset.path.starts_with("packages/") {
                continue;
            }
            let full_path = root.join(&asset.path);
            if !full_path.exists() {
                let kind_label = if asset.is_font { "Font asset" } else { "Asset" };
                diags.push(Diagnostic {
                    range: asset.range,
                    severity: Some(DiagnosticSeverity::WARNING),
                    code: Some(NumberOrString::String("missing-asset".to_string())),
                    source: Some("flutter-debugger".to_string()),
                    message: format!("{kind_label} path `{}` does not exist.", asset.path),
                    ..Default::default()
                });
            }
        }
    }

    if config.enabled {
        for eval in evals {
            if let Some(diag) = build_dependency_diagnostic(eval, config) {
                diags.push(diag);
            }
        }
    }

    diags
}

fn build_dependency_diagnostic(
    eval: &DependencyEvaluation,
    config: &PubspecConfig,
) -> Option<Diagnostic> {
    if eval.info.is_discontinued {
        let replacement = eval
            .info
            .replaced_by
            .as_deref()
            .map_or(String::new(), |r| format!(" Consider migrating to `{r}`."));
        return Some(Diagnostic {
            range: eval.dep.version_range,
            severity: Some(DiagnosticSeverity::WARNING),
            code: Some(NumberOrString::String("discontinued".to_string())),
            source: Some("flutter-debugger".to_string()),
            message: format!(
                "Package `{}` is discontinued on pub.dev.{replacement}",
                eval.dep.name
            ),
            tags: Some(vec![DiagnosticTag::DEPRECATED]),
            ..Default::default()
        });
    }

    if eval.current_retracted {
        let target = eval
            .latest_overall
            .as_ref()
            .map_or("a non-retracted version".to_string(), ToString::to_string);
        return Some(Diagnostic {
            range: eval.dep.version_range,
            severity: Some(DiagnosticSeverity::WARNING),
            code: Some(NumberOrString::String("retracted".to_string())),
            source: Some("flutter-debugger".to_string()),
            message: format!(
                "Current version of `{}` has been retracted on pub.dev. Upgrade to {target}.",
                eval.dep.name
            ),
            ..Default::default()
        });
    }

    let latest = eval.latest_overall.as_ref()?;
    let (severity, code, label) = match eval.upgrade_kind {
        UpgradeKind::UpToDate => return None,
        UpgradeKind::Major => (
            config.major_severity.to_lsp()?,
            "outdated-major",
            "New major version",
        ),
        UpgradeKind::Minor => (
            config.minor_patch_severity.to_lsp()?,
            "outdated-minor",
            "New minor version",
        ),
        UpgradeKind::Patch => (
            config.minor_patch_severity.to_lsp()?,
            "outdated-patch",
            "New patch version",
        ),
        UpgradeKind::Prerelease => (
            config.minor_patch_severity.to_lsp()?,
            "outdated-prerelease",
            "New prerelease",
        ),
    };

    let current_display = eval
        .baseline_version
        .as_ref()
        .map_or_else(|| "current".to_string(), ToString::to_string);

    let sdk_note = if eval.latest_sdk_incompatible {
        eval.latest_sdk_requirement
            .as_deref()
            .map_or(String::new(), |sdk| format!(" (requires Dart SDK {sdk})"))
    } else {
        String::new()
    };

    Some(Diagnostic {
        range: eval.dep.version_range,
        severity: Some(severity),
        code: Some(NumberOrString::String(code.to_string())),
        source: Some("flutter-debugger".to_string()),
        message: format!("{label}: {current_display} → {latest}{sdk_note}"),
        ..Default::default()
    })
}

/// Builds clickable `DocumentLink`s for:
/// 1. Hosted package names -> `<hosted_url>/packages/<name>`
/// 2. Local `path:` dependencies -> `<path>/pubspec.yaml` (or directory)
/// 3. `flutter.assets` & `flutter.fonts` -> local disk file/directory
#[must_use]
pub fn build_pubspec_document_links(
    parsed: &ParsedPubspec,
    project_root: Option<&Path>,
    default_hosted_url: &str,
    include_hosted_links: bool,
) -> Vec<DocumentLink> {
    let default_base = default_hosted_url.trim_end_matches('/');
    let mut links = Vec::new();

    for dep in &parsed.dependencies {
        match &dep.source {
            DependencySource::Hosted { url } if include_hosted_links => {
                let hosted = url.as_deref().unwrap_or(default_base).trim_end_matches('/');
                let url_str = format!("{hosted}/packages/{}", dep.name);
                if let Ok(target) = url_str.parse::<Uri>() {
                    links.push(DocumentLink {
                        range: dep.name_range,
                        target: Some(target),
                        tooltip: Some(format!("Open `{}` on pub.dev", dep.name)),
                        data: None,
                    });
                }
            }
            DependencySource::Path { path, range } => {
                if let Some(root) = project_root {
                    let pkg_dir = root.join(path);
                    let target_path = if pkg_dir.join("pubspec.yaml").is_file() {
                        pkg_dir.join("pubspec.yaml")
                    } else {
                        pkg_dir
                    };
                    if target_path.exists()
                        && let Some(target_uri) = file_path_to_uri(&target_path)
                    {
                        links.push(DocumentLink {
                            range: *range,
                            target: Some(target_uri),
                            tooltip: Some(format!("Open local package `{}`", dep.name)),
                            data: None,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(root) = project_root {
        for asset in &parsed.assets {
            let full_path = root.join(&asset.path);
            if full_path.exists()
                && let Some(target_uri) = file_path_to_uri(&full_path)
            {
                links.push(DocumentLink {
                    range: asset.range,
                    target: Some(target_uri),
                    tooltip: Some(format!("Open {}", asset.path)),
                    data: None,
                });
            }
        }
    }

    links
}

/// Builds actionable inline `InlayHint`s at the end of hosted dependency lines.
/// Clicking an outdated or retracted hint applies the version upgrade directly via `text_edits`.
#[must_use]
pub fn build_pubspec_inlay_hints(
    evals: &[DependencyEvaluation],
    visible_range: Range,
    config: &PubspecConfig,
) -> Vec<InlayHint> {
    if !config.enabled || !config.inlay_hints {
        return Vec::new();
    }

    evals
        .iter()
        .filter(|e| {
            e.dep.hint_position.line >= visible_range.start.line
                && e.dep.hint_position.line <= visible_range.end.line
        })
        .filter_map(|eval| build_single_inlay_hint(eval, config))
        .collect()
}

fn build_single_inlay_hint(
    eval: &DependencyEvaluation,
    config: &PubspecConfig,
) -> Option<InlayHint> {
    let (label, text_edits, tooltip) = if eval.info.is_discontinued {
        let lbl = match &eval.info.replaced_by {
            Some(rep) => format!("⚠ discontinued (use {rep})"),
            None => "⚠ discontinued".to_string(),
        };
        (
            lbl,
            None,
            Some(InlayHintTooltip::String(
                "Package is discontinued on pub.dev".to_string(),
            )),
        )
    } else if eval.current_retracted {
        match eval.preferred_upgrade_edit() {
            Some((latest, new_text)) => (
                format!("⚠ retracted → {latest}"),
                Some(vec![TextEdit {
                    range: eval.dep.version_range,
                    new_text: new_text.clone(),
                }]),
                Some(InlayHintTooltip::String(format!(
                    "Click to upgrade `{}` to {new_text}",
                    eval.dep.name
                ))),
            ),
            None => ("⚠ retracted".to_string(), None, None),
        }
    } else if let Some(latest) = &eval.latest_overall {
        match eval.upgrade_kind {
            UpgradeKind::UpToDate => {
                if !config.show_up_to_date_hints {
                    return None;
                }
                ("✓ latest".to_string(), None, None)
            }
            UpgradeKind::Major => {
                let new_text =
                    format_upgraded_constraint(eval.dep.raw_constraint.as_deref(), latest);
                let lbl = if eval.latest_sdk_incompatible {
                    format!("⬆ {latest} (major, SDK)")
                } else {
                    format!("⬆ {latest} (major)")
                };
                (
                    lbl,
                    Some(vec![TextEdit {
                        range: eval.dep.version_range,
                        new_text: new_text.clone(),
                    }]),
                    Some(InlayHintTooltip::String(format!(
                        "Click to upgrade `{}` to {new_text} (major)",
                        eval.dep.name
                    ))),
                )
            }
            UpgradeKind::Minor | UpgradeKind::Patch | UpgradeKind::Prerelease => {
                let new_text =
                    format_upgraded_constraint(eval.dep.raw_constraint.as_deref(), latest);
                (
                    format!("⬆ {latest}"),
                    Some(vec![TextEdit {
                        range: eval.dep.version_range,
                        new_text: new_text.clone(),
                    }]),
                    Some(InlayHintTooltip::String(format!(
                        "Click to upgrade `{}` to {new_text}",
                        eval.dep.name
                    ))),
                )
            }
        }
    } else {
        return None;
    };

    Some(InlayHint {
        position: eval.dep.hint_position,
        label: InlayHintLabel::String(label),
        kind: None,
        text_edits,
        tooltip,
        padding_left: Some(true),
        padding_right: Some(false),
        data: None,
    })
}

/// Renders rich Markdown hover for a hosted pub dependency (metadata, score, versions, changelog).
#[must_use]
pub fn build_hosted_hover_markdown(
    eval: &DependencyEvaluation,
    changelog_entries: Option<&[ChangelogEntry]>,
    config: &PubspecConfig,
) -> String {
    let mut md = String::new();
    md.push_str(&format!("### `{}`", eval.dep.name));
    if eval.info.is_discontinued {
        md.push_str("  *(Discontinued)*");
    } else if eval.current_retracted {
        md.push_str("  *(Current version retracted)*");
    }
    md.push('\n');

    if let Some(desc) = eval
        .info
        .description
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        md.push_str(&format!("\n{}\n", desc.trim()));
    }

    if let Some(score) = &eval.info.score {
        let mut badges = Vec::new();
        if let (Some(granted), Some(max)) = (score.granted_points, score.max_points) {
            badges.push(format!("Pub Points: **{granted}/{max}**"));
        }
        if let Some(likes) = score.like_count {
            badges.push(format!("Likes: **{likes}**"));
        }
        if let Some(dl) = score.download_count_30_days {
            badges.push(format!("30d Downloads: **{}**", format_compact_count(dl)));
        }
        if !badges.is_empty() {
            md.push_str(&format!("\n{}\n", badges.join(" · ")));
        }
    }

    md.push('\n');
    let declared = eval.dep.raw_constraint.as_deref().unwrap_or("any");
    md.push_str(&format!("- **Declared**: `{declared}`\n"));
    if let Some(locked) = &eval.locked_version {
        md.push_str(&format!("- **Locked (`pubspec.lock`)**: `{locked}`\n"));
    }
    if let Some(compat) = &eval.latest_compatible {
        md.push_str(&format!("- **Latest Compatible**: `{compat}`\n"));
    }
    if let Some(latest) = &eval.latest_overall {
        let sdk_warn = if eval.latest_sdk_incompatible {
            eval.latest_sdk_requirement
                .as_deref()
                .map_or(String::new(), |s| format!(" *(requires Dart SDK `{s}`)*"))
        } else {
            String::new()
        };
        md.push_str(&format!("- **Latest**: `{latest}`{sdk_warn}\n"));
    }
    if let Some(rep) = &eval.info.replaced_by {
        md.push_str(&format!(
            "- **Replaced By**: [`{rep}`]({}/packages/{rep})\n",
            eval.info.hosted_url
        ));
    }

    if let Some((target, new_text)) = eval.preferred_upgrade_edit() {
        md.push_str(&format!(
            "\n> **Quick Upgrade → `{new_text}`**: Click the inline `⬆ {target}` hint or press `Cmd+.` (`Ctrl+.`)\n"
        ));
    }

    md.push_str("\n#### Changelog\n\n");
    match changelog_entries {
        Some(entries) if !entries.is_empty() => {
            let selected = select_relevant_entries(
                entries,
                eval.baseline_version.as_ref(),
                eval.latest_overall.as_ref(),
                config.max_changelog_versions,
            );
            for entry in selected {
                md.push_str(&format!("**{}**\n\n", entry.heading));
                if entry.body_markdown.is_empty() {
                    md.push_str("*(No release notes provided)*\n\n");
                } else {
                    md.push_str(&entry.body_markdown);
                    md.push_str("\n\n");
                }
            }
        }
        Some(_) => {
            md.push_str("*(No changelog entries found on pub.dev)*\n\n");
        }
        None => {
            md.push_str("*(Changelog unavailable right now — view online below)*\n\n");
        }
    }

    let mut links = vec![
        format!("[pub.dev]({})", eval.info.package_page_url()),
        format!("[Full Changelog]({})", eval.info.changelog_page_url()),
    ];
    if let Some(repo) = eval
        .info
        .repository
        .as_deref()
        .or(eval.info.homepage.as_deref())
    {
        links.push(format!("[Repository]({repo})"));
    }
    md.push_str(&links.join(" · "));
    md
}

/// Renders rich Markdown hover for a local `path:` dependency, reading its sibling `pubspec.yaml`.
#[must_use]
pub fn build_local_path_hover_markdown(
    dep: &ParsedDependency,
    rel_path: &str,
    project_root: Option<&Path>,
) -> String {
    let mut md = format!("### `{}` *(Local Path Dependency)*\n\n", dep.name);
    let Some(root) = project_root else {
        md.push_str(&format!("- **Path**: `{rel_path}`\n"));
        return md;
    };

    let full_dir = root.join(rel_path);
    let pubspec_file = full_dir.join("pubspec.yaml");
    md.push_str(&format!("- **Path**: `{}`\n", full_dir.display()));

    if !full_dir.exists() {
        md.push_str("- **Status**: Missing directory\n");
        return md;
    }

    if let Ok(content) = fs::read_to_string(&pubspec_file) {
        let local_parsed = parse_pubspec(&content);
        if let Some(name) = local_parsed.project_name {
            md.push_str(&format!("- **Package Name**: `{name}`\n"));
        }
        if let Some(ver) = local_parsed.project_version {
            md.push_str(&format!("- **Version**: `{ver}`\n"));
        }
        if let Some(sdk) = local_parsed.environment_sdk_raw {
            md.push_str(&format!("- **Dart SDK**: `{sdk}`\n"));
        }
        if let Some(desc) = local_parsed.project_description {
            md.push_str(&format!("\n{desc}\n"));
        }
    } else {
        md.push_str("- **Status**: Directory exists *(no `pubspec.yaml` found)*\n");
    }

    md
}

/// Renders concise Markdown hover for SDK or Git dependencies.
#[must_use]
pub fn build_sdk_or_git_hover_markdown(dep: &ParsedDependency) -> Option<String> {
    match &dep.source {
        DependencySource::Sdk(sdk) => Some(format!(
            "### `{}` *(SDK Dependency)*\n\n- **SDK**: `{sdk}`\n- **Section**: `{}`\n",
            dep.name,
            dep.section.as_str()
        )),
        DependencySource::Git { url } => {
            let url_line = url
                .as_deref()
                .map_or(String::new(), |u| format!("- **Repository**: `{u}`\n"));
            Some(format!(
                "### `{}` *(Git Dependency)*\n\n{url_line}- **Section**: `{}`\n",
                dep.name,
                dep.section.as_str()
            ))
        }
        _ => None,
    }
}

/// Renders Markdown hover for a `flutter.assets` or `flutter.fonts` entry.
#[must_use]
pub fn build_asset_hover_markdown(
    asset: &FlutterAssetEntry,
    project_root: Option<&Path>,
) -> String {
    let kind = if asset.is_font {
        "Flutter Font Asset"
    } else {
        "Flutter Asset"
    };
    let mut md = format!("### `{}` *({kind})*\n\n", asset.path);

    if asset.path.starts_with("packages/") {
        md.push_str("- **Source**: Package-provided asset\n");
        return md;
    }

    let Some(root) = project_root else {
        return md;
    };
    let full = root.join(&asset.path);
    md.push_str(&format!("- **Resolved Path**: `{}`\n", full.display()));

    match fs::metadata(&full) {
        Ok(meta) if meta.is_file() => {
            md.push_str(&format!(
                "- **Status**: Found file ({})\n",
                format_byte_size(meta.len())
            ));
        }
        Ok(meta) if meta.is_dir() => {
            let count = fs::read_dir(&full)
                .map(|rd| rd.flatten().filter(|e| e.path().is_file()).count())
                .unwrap_or(0);
            md.push_str(&format!("- **Status**: Found directory ({count} file(s))\n"));
        }
        _ => {
            md.push_str("- **Status**: Missing on disk\n");
        }
    }

    md
}

/// Builds semver completion items for a hosted dependency's version value in `pubspec.yaml`.
#[must_use]
pub fn build_version_completion_items(
    ctx: &VersionCompletionContext,
    info: &PackageInfo,
    locked_version: Option<&str>,
    project_sdk: Option<&VersionConstraint>,
    config: &PubspecConfig,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    let max_items = 30usize;
    let mut latest_stable_seen = false;

    for v in &info.versions {
        if v.retracted {
            continue;
        }
        let Some(parsed) = parse_dart_version(&v.version) else {
            continue;
        };
        if !parsed.pre.is_empty() && !config.include_prerelease && !ctx.filter_prefix.contains('-')
        {
            continue;
        }
        if !ctx.filter_prefix.is_empty() && !v.version.starts_with(&ctx.filter_prefix) {
            continue;
        }

        let sdk_ok = is_sdk_compatible(project_sdk, v.sdk_constraint.as_deref());
        let is_latest = !latest_stable_seen && parsed.pre.is_empty();
        if is_latest {
            latest_stable_seen = true;
        }

        let raw_ver = if ctx.use_caret {
            format!("^{}", v.version)
        } else {
            v.version.clone()
        };
        let insert_text = match ctx.quote {
            Some(q) => format!("{q}{raw_ver}{q}"),
            None => raw_ver.clone(),
        };

        let mut tags = Vec::new();
        if is_latest {
            tags.push("latest".to_string());
        }
        if locked_version == Some(v.version.as_str()) {
            tags.push("locked".to_string());
        }
        if !sdk_ok {
            if let Some(sdk) = &v.sdk_constraint {
                tags.push(format!("requires Dart SDK {sdk}"));
            } else {
                tags.push("SDK mismatch".to_string());
            }
        }
        if let Some(pub_date) = v.published.as_deref() {
            let short_date = pub_date.split('T').next().unwrap_or(pub_date);
            tags.push(short_date.to_string());
        }

        let detail = if tags.is_empty() {
            None
        } else {
            Some(tags.join(" · "))
        };

        let idx = items.len();
        items.push(CompletionItem {
            label: raw_ver,
            kind: Some(CompletionItemKind::VALUE),
            detail,
            documentation: info.description.as_ref().map(|d| {
                Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: format!("**`{}` {}**\n\n{}", info.name, v.version, d.trim()),
                })
            }),
            sort_text: Some(format!("{idx:05}")),
            filter_text: Some(v.version.clone()),
            preselect: Some(is_latest && sdk_ok),
            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                range: ctx.replace_range,
                new_text: insert_text,
            })),
            ..Default::default()
        });

        if items.len() >= max_items {
            break;
        }
    }

    items
}

/// Builds clickable CodeLens items at the top of `pubspec.yaml` / `pubspec_overrides.yaml`:
/// - `Refresh (N updates)` (`flutterDebugger.refreshPubVersions`)
/// - `<flutter|dart> pub get`, `<flutter|dart> pub upgrade`, `<flutter|dart> pub outdated`
#[must_use]
pub fn build_pubspec_code_lenses(
    uri: &Uri,
    pubspec: &ParsedPubspec,
    project_kind: ProjectKind,
    evals: &[DependencyEvaluation],
    is_refreshing: bool,
    running_pub_cmd: Option<&str>,
    config: &PubspecConfig,
) -> Vec<CodeLens> {
    if !config.enabled || !config.code_lens || !pubspec.is_valid_dart_project {
        return Vec::new();
    }

    let anchor_range = pubspec.section_headers.first().map_or_else(
        || Range::new(Position::new(0, 0), Position::new(0, 0)),
        |h| h.range,
    );

    let uri_arg = json!(uri.as_str());
    let tool = if pubspec.is_flutter_project || project_kind == ProjectKind::Flutter {
        "flutter"
    } else {
        "dart"
    };

    let make_pub_lens = |subcmd: &str, cmd_id: &str| -> CodeLens {
        let label = format!("{tool} pub {subcmd}");
        let title = if running_pub_cmd == Some(label.as_str()) {
            format!("{label}…")
        } else {
            label
        };
        CodeLens {
            range: anchor_range,
            command: Some(Command {
                title,
                command: cmd_id.to_string(),
                arguments: Some(vec![uri_arg.clone()]),
            }),
            data: None,
        }
    };

    let mut lenses = Vec::with_capacity(4);

    if config.code_lens_refresh {
        let refresh_title = if is_refreshing {
            "Checking pub.dev…".to_string()
        } else {
            let updates = evals.iter().filter(|e| e.has_update()).count();
            let discontinued = evals.iter().filter(|e| e.info.is_discontinued).count();
            let update_word = if updates == 1 { "update" } else { "updates" };
            if updates == 0 && discontinued == 0 {
                format!("Refresh ({} checked · up to date)", evals.len())
            } else if discontinued > 0 {
                format!("Refresh ({updates} {update_word}, {discontinued} discontinued)")
            } else {
                format!("Refresh ({updates} {update_word})")
            }
        };
        lenses.push(CodeLens {
            range: anchor_range,
            command: Some(Command {
                title: refresh_title,
                command: CMD_PUB_REFRESH.to_string(),
                arguments: Some(vec![uri_arg.clone()]),
            }),
            data: None,
        });
    }

    if config.code_lens_pub_get {
        lenses.push(make_pub_lens("get", CMD_PUB_GET));
    }
    if config.code_lens_pub_upgrade {
        lenses.push(make_pub_lens("upgrade", CMD_PUB_UPGRADE));
    }
    if config.code_lens_pub_outdated {
        lenses.push(make_pub_lens("outdated", CMD_PUB_OUTDATED));
    }

    lenses
}

/// Builds Quick Fix and Source Code Actions for `pubspec.yaml` / `pubspec_overrides.yaml`:
/// 1. Single-dependency compatible and major upgrades on cursor line;
/// 2. Batch compatible upgrade (when 2+ packages have compatible updates);
/// 3. Batch latest upgrade including major versions (non-preferred, when 2+ updates include a major);
/// 4. Manual refresh source action.
#[must_use]
pub fn build_pubspec_code_actions(
    uri: &Uri,
    evals: &[DependencyEvaluation],
    requested_range: Range,
) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();

    // 1. Per-dependency upgrade quick fixes for the line under cursor.
    for eval in evals {
        if !ranges_overlap_line(eval.dep.name_range, eval.dep.version_range, requested_range) {
            continue;
        }
        if !matches!(eval.dep.source, DependencySource::Hosted { .. }) {
            continue;
        }

        let mut offered_versions: Vec<String> = Vec::new();

        if let Some(compat) = &eval.latest_compatible
            && eval
                .baseline_version
                .as_ref()
                .is_none_or(|b| compare_dart_versions(compat, b) == Ordering::Greater)
        {
            let new_text = format_upgraded_constraint(eval.dep.raw_constraint.as_deref(), compat);
            let title = format!("Upgrade `{}` to {new_text} (compatible)", eval.dep.name);
            actions.push(make_edit_action(
                uri,
                title,
                eval.dep.version_range,
                new_text,
                true,
            ));
            offered_versions.push(compat.to_string());
        }

        if let Some(latest) = &eval.latest_overall
            && (eval.has_update() || eval.current_retracted)
            && !offered_versions.contains(&latest.to_string())
        {
            let new_text = format_upgraded_constraint(eval.dep.raw_constraint.as_deref(), latest);
            let kind_tag = match eval.upgrade_kind {
                UpgradeKind::Major => " (major)",
                UpgradeKind::Prerelease => " (prerelease)",
                _ => "",
            };
            let title = format!("Upgrade `{}` to {new_text}{kind_tag}", eval.dep.name);
            let is_preferred =
                eval.upgrade_kind != UpgradeKind::Major && offered_versions.is_empty();
            actions.push(make_edit_action(
                uri,
                title,
                eval.dep.version_range,
                new_text,
                is_preferred,
            ));
        }
    }

    // 2. Batch compatible upgrade action if 2+ packages have compatible updates.
    let batch_compat_edits: Vec<TextEdit> = evals
        .iter()
        .filter_map(|eval| {
            let compat = eval.latest_compatible.as_ref()?;
            let base = eval.baseline_version.as_ref()?;
            if compare_dart_versions(compat, base) != Ordering::Greater {
                return None;
            }
            let raw = eval.dep.raw_constraint.as_deref()?;
            let new_text = format_upgraded_constraint(Some(raw), compat);
            Some(TextEdit {
                range: eval.dep.version_range,
                new_text,
            })
        })
        .collect();

    if batch_compat_edits.len() >= 2 {
        let mut changes = HashMap::new();
        changes.insert(uri.clone(), batch_compat_edits);
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: "Flutter Debugger: Upgrade all dependencies to latest compatible".to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            is_preferred: Some(false),
            edit: Some(WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            }),
            ..Default::default()
        }));
    }

    // 3. Batch upgrade ALL dependencies to latest (including major), clearly non-preferred.
    let mut has_major_update = false;
    let batch_all_edits: Vec<TextEdit> = evals
        .iter()
        .filter_map(|eval| {
            let (_, new_text) = eval.preferred_upgrade_edit()?;
            if eval.upgrade_kind == UpgradeKind::Major {
                has_major_update = true;
            }
            Some(TextEdit {
                range: eval.dep.version_range,
                new_text,
            })
        })
        .collect();

    if batch_all_edits.len() >= 2 && has_major_update {
        let mut changes = HashMap::new();
        changes.insert(uri.clone(), batch_all_edits);
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: "Flutter Debugger: Upgrade all dependencies to latest (including major)"
                .to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            is_preferred: Some(false),
            edit: Some(WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            }),
            ..Default::default()
        }));
    }

    // 4. Manual refresh source action.
    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
        title: "Flutter Debugger: Refresh dependency versions from pub.dev".to_string(),
        kind: Some(CodeActionKind::SOURCE),
        command: Some(Command {
            title: "Refresh dependency versions".to_string(),
            command: CMD_PUB_REFRESH.to_string(),
            arguments: Some(vec![json!(uri.as_str())]),
        }),
        ..Default::default()
    }));

    actions
}

fn make_edit_action(
    uri: &Uri,
    title: String,
    range: Range,
    new_text: String,
    is_preferred: bool,
) -> CodeActionOrCommand {
    let mut changes = HashMap::new();
    changes.insert(uri.clone(), vec![TextEdit { range, new_text }]);

    CodeActionOrCommand::CodeAction(CodeAction {
        title,
        kind: Some(CodeActionKind::QUICKFIX),
        is_preferred: Some(is_preferred),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }),
        ..Default::default()
    })
}

fn ranges_overlap_line(name_range: Range, version_range: Range, query: Range) -> bool {
    let min_line = name_range.start.line.min(version_range.start.line);
    let max_line = name_range.end.line.max(version_range.end.line);
    query.start.line <= max_line && query.end.line >= min_line
}

fn format_compact_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn format_byte_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::pubspec::parser::completion_context_at;
    use crate::lsp::pubspec::registry::VersionSummary;

    fn sample_pkg(name: &str, versions: &[(&str, bool, Option<&str>)]) -> PackageInfo {
        PackageInfo {
            name: name.to_string(),
            hosted_url: "https://pub.dev".to_string(),
            fetched_at_unix: 1_700_000_000,
            is_discontinued: false,
            replaced_by: None,
            description: Some(format!("Package {name} description")),
            repository: Some(format!("https://github.com/example/{name}")),
            homepage: None,
            latest_stable: versions.first().map(|v| v.0.to_string()),
            latest_prerelease: None,
            versions: versions
                .iter()
                .map(|(ver, retracted, sdk)| VersionSummary {
                    version: (*ver).to_string(),
                    retracted: *retracted,
                    published: Some("2026-01-15T00:00:00Z".to_string()),
                    sdk_constraint: sdk.map(ToString::to_string),
                })
                .collect(),
            score: None,
        }
    }

    #[test]
    fn merges_missing_asset_and_strict_dependency_diagnostics_plus_actions_and_completions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local_pkg = dir.path().join("packages/core_ui");
        fs::create_dir_all(&local_pkg).expect("mkdir local_pkg");
        fs::write(
            local_pkg.join("pubspec.yaml"),
            "name: core_ui\nversion: 0.4.2\ndescription: Shared UI kit\n",
        )
        .expect("write local pubspec");
        fs::create_dir_all(dir.path().join("assets")).expect("mkdir assets");
        fs::write(dir.path().join("assets/logo.png"), vec![0u8; 2048]).expect("write logo");

        let yaml = concat!(
            "name: demo\n",
            "environment:\n",
            "  sdk: ^3.5.0\n",
            "dependencies:\n",
            "  flutter:\n",
            "    sdk: flutter\n",
            "  dio: ^5.1.0\n",
            "  http: ^1.2.0\n",
            "  core_ui:\n",
            "    path: packages/core_ui\n",
            "flutter:\n",
            "  assets:\n",
            "    - assets/logo.png\n",
            "    - assets/missing.png\n",
        );

        let parsed = parse_pubspec(yaml);
        let config = PubspecConfig::default();

        let dio_dep = parsed
            .dependencies
            .iter()
            .find(|d| d.name == "dio")
            .expect("dio")
            .clone();
        let http_dep = parsed
            .dependencies
            .iter()
            .find(|d| d.name == "http")
            .expect("http")
            .clone();

        let dio_info = sample_pkg(
            "dio",
            &[
                ("6.0.0", false, Some("^3.4.0")),
                ("5.8.0", false, Some("^3.0.0")),
                ("5.7.0", true, Some("^3.0.0")),
            ],
        );
        let http_info = sample_pkg("http", &[("1.3.0", false, Some("^3.0.0"))]);

        let dio_eval = DependencyEvaluation::evaluate(
            dio_dep,
            dio_info.clone(),
            Some("5.1.0".to_string()),
            parsed.environment_sdk.as_ref(),
            &config,
        );
        let http_eval = DependencyEvaluation::evaluate(
            http_dep,
            http_info,
            Some("1.2.0".to_string()),
            parsed.environment_sdk.as_ref(),
            &config,
        );
        let evals = vec![dio_eval.clone(), http_eval];

        // 1. Unified diagnostics: missing asset (WARNING) + dio major (WARNING) + http minor (INFO in strict mode)
        let diags = build_pubspec_diagnostics(&parsed, &evals, Some(dir.path()), &config);
        assert_eq!(diags.len(), 3);
        assert!(diags.iter().any(|d| {
            d.code == Some(NumberOrString::String("missing-asset".to_string()))
                && d.severity == Some(DiagnosticSeverity::WARNING)
        }));
        assert!(diags.iter().any(|d| {
            d.code == Some(NumberOrString::String("outdated-major".to_string()))
                && d.severity == Some(DiagnosticSeverity::WARNING)
        }));
        assert!(diags.iter().any(|d| {
            d.code == Some(NumberOrString::String("outdated-minor".to_string()))
                && d.severity == Some(DiagnosticSeverity::INFORMATION)
        }));

        // 2. Clickable InlayHints
        let hints = build_pubspec_inlay_hints(
            &evals,
            Range::new(Position::new(0, 0), Position::new(20, 0)),
            &config,
        );
        assert_eq!(hints.len(), 2);
        assert!(hints[0].text_edits.is_some());

        // 3. CodeActions include single compatible, single major, batch compatible, batch major (non-preferred), and refresh
        let uri: Uri = "file:///tmp/demo/pubspec.yaml".parse().expect("uri");
        let actions = build_pubspec_code_actions(
            &uri,
            &evals,
            Range::new(Position::new(6, 2), Position::new(6, 5)),
        );
        let titles: Vec<&str> = actions
            .iter()
            .filter_map(|a| match a {
                CodeActionOrCommand::CodeAction(ca) => Some(ca.title.as_str()),
                _ => None,
            })
            .collect();
        assert!(titles.iter().any(|t| t.contains("Upgrade `dio` to ^5.8.0 (compatible)")));
        assert!(titles.iter().any(|t| t.contains("Upgrade `dio` to ^6.0.0 (major)")));
        assert!(titles.iter().any(|t| t.contains("latest compatible")));
        assert!(titles.iter().any(|t| t.contains("latest (including major)")));
        assert!(titles.iter().any(|t| t.contains("Refresh dependency versions")));

        // Verify batch major action is explicitly non-preferred
        let batch_major = actions.iter().find_map(|a| match a {
            CodeActionOrCommand::CodeAction(ca)
                if ca.title.contains("including major") =>
            {
                Some(ca)
            }
            _ => None,
        });
        assert_eq!(batch_major.expect("batch major").is_preferred, Some(false));

        // 4. Local path hover & asset hover
        let local_dep = parsed
            .dependencies
            .iter()
            .find(|d| d.name == "core_ui")
            .expect("core_ui");
        let local_hover =
            build_local_path_hover_markdown(local_dep, "packages/core_ui", Some(dir.path()));
        assert!(local_hover.contains("core_ui"));
        assert!(local_hover.contains("0.4.2"));
        assert!(local_hover.contains("Shared UI kit"));

        let existing_asset_hover = build_asset_hover_markdown(&parsed.assets[0], Some(dir.path()));
        assert!(existing_asset_hover.contains("2.0 KB"));
        let missing_asset_hover = build_asset_hover_markdown(&parsed.assets[1], Some(dir.path()));
        assert!(missing_asset_hover.contains("Missing on disk"));

        // 5. Version completions exclude retracted 5.7.0 and preserve newest-first ordering
        let ctx = completion_context_at(yaml, &parsed, Position::new(6, 13)).expect("ctx");
        let completions = build_version_completion_items(
            &ctx,
            &dio_info,
            Some("5.1.0"),
            parsed.environment_sdk.as_ref(),
            &config,
        );
        // `dio: ^5.1.0` -> filter_prefix = "5.1.0", none start with 5.1.0; test fresh prefix `^`
        assert!(completions.is_empty());

        let yaml_typing = "name: demo\ndependencies:\n  dio: ^\n";
        let parsed_typing = parse_pubspec(yaml_typing);
        let ctx_typing =
            completion_context_at(yaml_typing, &parsed_typing, Position::new(2, 8)).expect("ctx");
        let items = build_version_completion_items(
            &ctx_typing,
            &dio_info,
            Some("5.8.0"),
            parsed.environment_sdk.as_ref(),
            &config,
        );
        assert_eq!(items.len(), 2); // 5.7.0 is retracted and excluded
        assert_eq!(items[0].label, "^6.0.0");
        assert_eq!(items[0].preselect, Some(true));
        assert_eq!(items[1].label, "^5.8.0");
        assert!(items[1].detail.as_deref().unwrap_or("").contains("locked"));

        // 6. CodeLens switches to flutter and respects per-button toggles
        let lenses = build_pubspec_code_lenses(
            &uri,
            &parsed,
            ProjectKind::Flutter,
            &evals,
            false,
            Some("flutter pub get"),
            &config,
        );
        assert_eq!(lenses.len(), 4);
        assert_eq!(
            lenses[0].command.as_ref().expect("c").title,
            "Refresh (2 updates)"
        );
        assert_eq!(
            lenses[1].command.as_ref().expect("c").title,
            "flutter pub get…"
        );
    }
}
