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
use crate::lsp::pubspec::ignore::{PackageIgnoreRules, PubspecIgnoreConfig};
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
pub const CMD_PUB_IGNORE: &str = "flutterDebugger.ignorePubUpdate";
pub const CMD_PUB_UNIGNORE: &str = "flutterDebugger.unignorePubUpdate";
pub const CMD_PUB_OPEN_IGNORE_FILE: &str = "flutterDebugger.openPubIgnoreFile";

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
    if eval.ignore_rules.is_all_ignored() {
        return None;
    }

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

    let latest = eval.latest_overall.as_ref();
    let (severity, code, label) = match eval.upgrade_kind {
        UpgradeKind::UpToDate => {
            if let Some((locked, synced)) = eval.sync_to_lock_edit() {
                let severity = config.lock_drift_severity.to_lsp()?;
                let declared = eval.dep.raw_constraint.as_deref().unwrap_or("any");
                return Some(Diagnostic {
                    range: eval.dep.version_range,
                    severity: Some(severity),
                    code: Some(NumberOrString::String("lock-drift".to_string())),
                    source: Some("flutter-debugger".to_string()),
                    message: format!(
                        "Locked at {locked} in pubspec.lock (pubspec.yaml declares {declared}) — sync constraint to {synced}"
                    ),
                    ..Default::default()
                });
            }
            return None;
        }
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
    let latest = latest?;

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

    let drift_note = if eval.lock_ahead_version.is_some()
        && let Some(declared) = eval.dep.raw_constraint.as_deref()
    {
        format!(" (pubspec.yaml declares {declared})")
    } else {
        String::new()
    };

    Some(Diagnostic {
        range: eval.dep.version_range,
        severity: Some(severity),
        code: Some(NumberOrString::String(code.to_string())),
        source: Some("flutter-debugger".to_string()),
        message: format!("{label}: {current_display} → {latest}{sdk_note}{drift_note}"),
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
    if eval.ignore_rules.is_all_ignored() {
        return None;
    }

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
                if let Some((locked, synced)) = eval.sync_to_lock_edit() {
                    (
                        format!("⇄ {locked} (locked)"),
                        Some(vec![TextEdit {
                            range: eval.dep.version_range,
                            new_text: synced.clone(),
                        }]),
                        Some(InlayHintTooltip::String(format!(
                            "Locked at {locked} in pubspec.lock — click to sync `{}` to {synced}",
                            eval.dep.name
                        ))),
                    )
                } else {
                    if eval.muted_newer_version || !config.show_up_to_date_hints {
                        return None;
                    }
                    ("✓ latest".to_string(), None, None)
                }
            }
            UpgradeKind::Major => {
                let new_text =
                    format_upgraded_constraint(eval.dep.raw_constraint.as_deref(), latest);
                let suffix = if eval.latest_sdk_incompatible {
                    " (major, SDK)"
                } else {
                    " (major)"
                };
                let (lbl, tip) = match &eval.lock_ahead_version {
                    Some(locked) => (
                        format!("locked {locked} · ⬆ {latest}{suffix}"),
                        format!(
                            "Locked at {locked} in pubspec.lock — click to upgrade `{}` to {new_text} (major)",
                            eval.dep.name
                        ),
                    ),
                    None => (
                        format!("⬆ {latest}{suffix}"),
                        format!("Click to upgrade `{}` to {new_text} (major)", eval.dep.name),
                    ),
                };
                (
                    lbl,
                    Some(vec![TextEdit {
                        range: eval.dep.version_range,
                        new_text,
                    }]),
                    Some(InlayHintTooltip::String(tip)),
                )
            }
            UpgradeKind::Minor | UpgradeKind::Patch | UpgradeKind::Prerelease => {
                let new_text =
                    format_upgraded_constraint(eval.dep.raw_constraint.as_deref(), latest);
                let (lbl, tip) = match &eval.lock_ahead_version {
                    Some(locked) => (
                        format!("locked {locked} · ⬆ {latest}"),
                        format!(
                            "Locked at {locked} in pubspec.lock — click to upgrade `{}` to {new_text}",
                            eval.dep.name
                        ),
                    ),
                    None => (
                        format!("⬆ {latest}"),
                        format!("Click to upgrade `{}` to {new_text}", eval.dep.name),
                    ),
                };
                (
                    lbl,
                    Some(vec![TextEdit {
                        range: eval.dep.version_range,
                        new_text,
                    }]),
                    Some(InlayHintTooltip::String(tip)),
                )
            }
        }
    } else if let Some((locked, synced)) = eval.sync_to_lock_edit() {
        (
            format!("⇄ {locked} (locked)"),
            Some(vec![TextEdit {
                range: eval.dep.version_range,
                new_text: synced.clone(),
            }]),
            Some(InlayHintTooltip::String(format!(
                "Locked at {locked} in pubspec.lock — click to sync `{}` to {synced}",
                eval.dep.name
            ))),
        )
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
        let label = if eval.raw_latest_overall.as_ref() != Some(latest) {
            "Latest (unignored)"
        } else {
            "Latest"
        };
        md.push_str(&format!("- **{label}**: `{latest}`{sdk_warn}\n"));
    }
    if let Some(raw_latest) = &eval.raw_latest_overall
        && eval.latest_overall.as_ref() != Some(raw_latest)
    {
        let matched = eval
            .ignore_rules
            .matching_rule_display(raw_latest)
            .map_or_else(String::new, |r| format!(" *(ignored by `{r}`)*"));
        md.push_str(&format!(
            "- **Latest on pub.dev**: `{raw_latest}`{matched}\n"
        ));
    }
    if !eval.ignore_rules.is_empty() {
        md.push_str(&format!(
            "- **Ignored Rules**: `{}` *(from `{}`)*\n",
            eval.ignore_rules.summary(),
            eval.ignore_rules.source_label()
        ));
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
        if let Some((locked, synced)) = eval.sync_to_lock_edit() {
            md.push_str(&format!(
                "> **Lock ahead of `pubspec.yaml`**: `pubspec.lock` is at `{locked}` while `pubspec.yaml` declares `{declared}` (use `Cmd+.` to sync to `{synced}`).\n"
            ));
        }
    } else if let Some((locked, synced)) = eval.sync_to_lock_edit() {
        md.push_str(&format!(
            "\n> **Out of sync with `pubspec.lock`**: `pubspec.yaml` declares `{declared}`, while `pubspec.lock` is already at `{locked}`. Click `⇄ {locked} (locked)` or press `Cmd+.` (`Ctrl+.`) to sync to `{synced}`.\n"
        ));
    } else if eval.ignore_rules.from_file {
        md.push_str(
            "\n> Updates muted by `.zed/pubspec-ignore.json` — press `Cmd+.` (`Ctrl+.`) to unignore or edit rules.\n",
        );
    }

    md.push_str("\n#### Changelog\n\n");
    let changelog_from = if eval.lock_ahead_version.is_some() {
        eval.declared_version
            .as_ref()
            .or(eval.baseline_version.as_ref())
    } else {
        eval.baseline_version.as_ref()
    };
    let changelog_target = eval
        .latest_overall
        .as_ref()
        .or(eval.lock_ahead_version.as_ref())
        .or(eval.raw_latest_overall.as_ref());
    match changelog_entries {
        Some(entries) if !entries.is_empty() => {
            let selected = select_relevant_entries(
                entries,
                changelog_from,
                changelog_target,
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

/// Renders concise Markdown hover for a `*`-ignored hosted dependency when no cached
/// `pub.dev` metadata is available (avoiding unnecessary network traffic).
#[must_use]
pub fn build_ignored_fallback_hover_markdown(
    dep: &ParsedDependency,
    ignore_rules: &PackageIgnoreRules,
    locked_version: Option<&str>,
    hosted_url: &str,
) -> String {
    let mut md = format!("### `{}`  *(Ignored)*\n\n", dep.name);
    let declared = dep.raw_constraint.as_deref().unwrap_or("any");
    md.push_str(&format!("- **Declared**: `{declared}`\n"));
    if let Some(locked) = locked_version {
        md.push_str(&format!("- **Locked (`pubspec.lock`)**: `{locked}`\n"));
    }
    md.push_str(&format!(
        "- **Ignored Rules**: `{}` *(from `{}`)*\n",
        ignore_rules.summary(),
        ignore_rules.source_label()
    ));
    if ignore_rules.from_file {
        md.push_str(
            "\n> Updates muted by `.zed/pubspec-ignore.json` — press `Cmd+.` (`Ctrl+.`) to unignore or edit rules.\n",
        );
    }
    let base = hosted_url.trim_end_matches('/');
    md.push_str(&format!(
        "\n[pub.dev]({base}/packages/{}) · [Full Changelog]({base}/packages/{}/changelog)",
        dep.name, dep.name
    ));
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
    ignore_rules: &PackageIgnoreRules,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    let max_items = 30usize;
    let mut latest_stable_seen = false;
    let mut preselected_any = false;

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
        let is_ignored = ignore_rules.matches_version(&parsed);
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
        if is_ignored {
            tags.push("ignored".to_string());
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

        let should_preselect = !preselected_any && parsed.pre.is_empty() && sdk_ok && !is_ignored;
        if should_preselect {
            preselected_any = true;
        }

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
            preselect: Some(should_preselect),
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
            let to_sync = evals
                .iter()
                .filter(|e| !e.has_update() && e.lock_ahead_version.is_some())
                .count();
            let discontinued = evals
                .iter()
                .filter(|e| !e.ignore_rules.is_all_ignored() && e.info.is_discontinued)
                .count();
            let checked = evals
                .iter()
                .filter(|e| !e.ignore_rules.is_all_ignored())
                .count();
            let update_word = if updates == 1 { "update" } else { "updates" };
            if updates == 0 && to_sync == 0 && discontinued == 0 {
                format!("Refresh ({checked} checked · up to date)")
            } else {
                let mut parts = Vec::new();
                if updates > 0 {
                    parts.push(format!("{updates} {update_word}"));
                }
                if to_sync > 0 {
                    if updates == 0 && discontinued == 0 {
                        parts.push(format!("{to_sync} to sync with lock"));
                    } else {
                        parts.push(format!("{to_sync} to sync"));
                    }
                }
                if discontinued > 0 {
                    parts.push(format!("{discontinued} discontinued"));
                }
                format!("Refresh ({})", parts.join(", "))
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
        let upgrade_sub = if config.pub_upgrade_tighten {
            "upgrade --tighten"
        } else {
            "upgrade"
        };
        lenses.push(make_pub_lens(upgrade_sub, CMD_PUB_UPGRADE));
    }
    if config.code_lens_pub_outdated {
        lenses.push(make_pub_lens("outdated", CMD_PUB_OUTDATED));
    }

    lenses
}

/// Builds Quick Fix and Source Code Actions for `pubspec.yaml` / `pubspec_overrides.yaml`:
/// 1. Single-dependency compatible and major upgrades on cursor line;
/// 2. Fine-grained ignore actions (`=version`, `>=version`, `*`) & unignore action on cursor line;
/// 3. Batch compatible upgrade (when 2+ packages have compatible updates);
/// 4. Batch latest upgrade including major versions (non-preferred, when 2+ updates include a major);
/// 5. Open `.zed/pubspec-ignore.json` & manual refresh source actions.
#[must_use]
pub fn build_pubspec_code_actions(
    uri: &Uri,
    parsed: &ParsedPubspec,
    evals: &[DependencyEvaluation],
    ignore_config: &PubspecIgnoreConfig,
    requested_range: Range,
) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();
    let uri_str = uri.as_str().to_string();
    let mut cursor_handled_pkgs: Vec<String> = Vec::new();

    // 1. Per-dependency upgrade & ignore/unignore actions for the line under cursor.
    for eval in evals {
        if !ranges_overlap_line(eval.dep.name_range, eval.dep.version_range, requested_range) {
            continue;
        }
        if !matches!(eval.dep.source, DependencySource::Hosted { .. }) {
            continue;
        }
        cursor_handled_pkgs.push(eval.dep.name.clone());

        let mut offered_versions: Vec<String> = Vec::new();
        let mut offered_texts: Vec<String> = Vec::new();

        if !eval.ignore_rules.is_all_ignored() {
            if let Some(compat) = &eval.latest_compatible
                && eval
                    .baseline_version
                    .as_ref()
                    .is_none_or(|b| compare_dart_versions(compat, b) == Ordering::Greater)
            {
                let new_text =
                    format_upgraded_constraint(eval.dep.raw_constraint.as_deref(), compat);
                let title = format!("Upgrade `{}` to {new_text} (compatible)", eval.dep.name);
                actions.push(make_edit_action(
                    uri,
                    title,
                    eval.dep.version_range,
                    new_text.clone(),
                    true,
                ));
                offered_versions.push(compat.to_string());
                offered_texts.push(new_text);
            }

            if let Some(latest) = &eval.latest_overall
                && (eval.has_update() || eval.current_retracted)
                && !offered_versions.contains(&latest.to_string())
            {
                let new_text =
                    format_upgraded_constraint(eval.dep.raw_constraint.as_deref(), latest);
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
                    new_text.clone(),
                    is_preferred,
                ));
                offered_texts.push(new_text);
            }

            if let Some((_, synced_text)) = eval.sync_to_lock_edit()
                && !offered_texts.contains(&synced_text)
            {
                let title = format!("Sync `{}` to {synced_text} (from pubspec.lock)", eval.dep.name);
                let is_preferred = !eval.has_update() && offered_versions.is_empty();
                actions.push(make_edit_action(
                    uri,
                    title,
                    eval.dep.version_range,
                    synced_text,
                    is_preferred,
                ));
            }

            if eval.has_update()
                && let Some(latest) = &eval.latest_overall
            {
                let exact_rule = format!("={latest}");
                let from_rule = format!(">={latest}");
                actions.push(make_command_action(
                    format!("Ignore `{}` {latest}", eval.dep.name),
                    CodeActionKind::QUICKFIX,
                    CMD_PUB_IGNORE,
                    vec![json!(uri_str), json!(eval.dep.name), json!(exact_rule)],
                ));
                actions.push(make_command_action(
                    format!(
                        "Ignore `{}` >={latest} (this & newer versions)",
                        eval.dep.name
                    ),
                    CodeActionKind::QUICKFIX,
                    CMD_PUB_IGNORE,
                    vec![json!(uri_str), json!(eval.dep.name), json!(from_rule)],
                ));
                actions.push(make_command_action(
                    format!("Ignore all updates for `{}`", eval.dep.name),
                    CodeActionKind::QUICKFIX,
                    CMD_PUB_IGNORE,
                    vec![json!(uri_str), json!(eval.dep.name), json!("*")],
                ));
            } else {
                actions.push(make_command_action(
                    format!("Ignore all updates for `{}`", eval.dep.name),
                    CodeActionKind::SOURCE,
                    CMD_PUB_IGNORE,
                    vec![json!(uri_str), json!(eval.dep.name), json!("*")],
                ));
            }
        }

        if ignore_config.has_file_rule(&eval.dep.name) {
            actions.push(make_command_action(
                format!("Unignore `{}` (clear ignore rules)", eval.dep.name),
                CodeActionKind::QUICKFIX,
                CMD_PUB_UNIGNORE,
                vec![json!(uri_str), json!(eval.dep.name)],
            ));
        }
    }

    // Fallback for hosted dependencies on cursor line that have no evaluation (e.g. `*`-ignored without cache).
    for dep in &parsed.dependencies {
        if !matches!(dep.source, DependencySource::Hosted { .. })
            || cursor_handled_pkgs.contains(&dep.name)
            || !ranges_overlap_line(dep.name_range, dep.version_range, requested_range)
        {
            continue;
        }
        if ignore_config.has_file_rule(&dep.name) {
            actions.push(make_command_action(
                format!("Unignore `{}` (clear ignore rules)", dep.name),
                CodeActionKind::QUICKFIX,
                CMD_PUB_UNIGNORE,
                vec![json!(uri_str), json!(dep.name)],
            ));
        } else if !ignore_config.is_all_ignored(&dep.name) {
            actions.push(make_command_action(
                format!("Ignore all updates for `{}`", dep.name),
                CodeActionKind::SOURCE,
                CMD_PUB_IGNORE,
                vec![json!(uri_str), json!(dep.name), json!("*")],
            ));
        }
    }

    // 2. Batch sync all drifted constraints to `pubspec.lock` when 2+ packages have lock ahead of YAML.
    let batch_sync_edits: Vec<TextEdit> = evals
        .iter()
        .filter_map(|eval| {
            let (_, synced_text) = eval.sync_to_lock_edit()?;
            Some(TextEdit {
                range: eval.dep.version_range,
                new_text: synced_text,
            })
        })
        .collect();

    if batch_sync_edits.len() >= 2 {
        let count = batch_sync_edits.len();
        let mut changes = HashMap::new();
        changes.insert(uri.clone(), batch_sync_edits);
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: format!(
                "Flutter Debugger: Sync all dependencies to pubspec.lock ({count} packages)"
            ),
            kind: Some(CodeActionKind::QUICKFIX),
            is_preferred: Some(false),
            edit: Some(WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            }),
            ..Default::default()
        }));
    }

    // 3. Batch compatible upgrade action if 2+ packages have compatible updates.
    let batch_compat_edits: Vec<TextEdit> = evals
        .iter()
        .filter_map(|eval| {
            if eval.ignore_rules.is_all_ignored() {
                return None;
            }
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

    // 4. Open `.zed/pubspec-ignore.json` when any ignore file or rule is active.
    if ignore_config.has_ignore_file || ignore_config.has_any_rules() {
        actions.push(make_command_action(
            "Flutter Debugger: Open .zed/pubspec-ignore.json".to_string(),
            CodeActionKind::SOURCE,
            CMD_PUB_OPEN_IGNORE_FILE,
            vec![json!(uri_str)],
        ));
    }

    // 5. Manual refresh source action.
    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
        title: "Flutter Debugger: Refresh dependency versions from pub.dev".to_string(),
        kind: Some(CodeActionKind::SOURCE),
        command: Some(Command {
            title: "Refresh dependency versions".to_string(),
            command: CMD_PUB_REFRESH.to_string(),
            arguments: Some(vec![json!(uri_str)]),
        }),
        ..Default::default()
    }));

    actions
}

fn make_command_action(
    title: String,
    kind: CodeActionKind,
    command: &str,
    arguments: Vec<serde_json::Value>,
) -> CodeActionOrCommand {
    CodeActionOrCommand::CodeAction(CodeAction {
        title: title.clone(),
        kind: Some(kind),
        is_preferred: Some(false),
        command: Some(Command {
            title,
            command: command.to_string(),
            arguments: Some(arguments),
        }),
        ..Default::default()
    })
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
            PackageIgnoreRules::default(),
        );
        let http_eval = DependencyEvaluation::evaluate(
            http_dep,
            http_info,
            Some("1.2.0".to_string()),
            parsed.environment_sdk.as_ref(),
            &config,
            PackageIgnoreRules::default(),
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

        // 3. CodeActions include single compatible, single major, ignore rules, batch compatible, batch major (non-preferred), and refresh
        let uri: Uri = "file:///tmp/demo/pubspec.yaml".parse().expect("uri");
        let empty_ignore = PubspecIgnoreConfig::default();
        let actions = build_pubspec_code_actions(
            &uri,
            &parsed,
            &evals,
            &empty_ignore,
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
        assert!(titles.iter().any(|t| t.contains("Ignore `dio` 6.0.0")));
        assert!(titles.iter().any(|t| t.contains("Ignore `dio` >=6.0.0")));
        assert!(titles.iter().any(|t| t.contains("Ignore all updates for `dio`")));
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
        let no_rules = PackageIgnoreRules::default();
        let completions = build_version_completion_items(
            &ctx,
            &dio_info,
            Some("5.1.0"),
            parsed.environment_sdk.as_ref(),
            &config,
            &no_rules,
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
            &no_rules,
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

    #[test]
    fn enforces_exact_from_version_and_all_ignore_semantics() {
        use crate::lsp::pubspec::ignore::IgnoreRule;

        let yaml = concat!(
            "name: demo\n",
            "environment:\n",
            "  sdk: ^3.5.0\n",
            "dependencies:\n",
            "  dio: ^5.4.0\n",
            "  legacy: ^1.0.0\n",
        );
        let parsed = parse_pubspec(yaml);
        let config = PubspecConfig::default();
        let dio_dep = parsed.dependencies[0].clone();
        let legacy_dep = parsed.dependencies[1].clone();

        let dio_info = sample_pkg(
            "dio",
            &[
                ("6.1.0", false, Some("^3.0.0")),
                ("6.0.0", false, Some("^3.0.0")),
                ("5.9.0", false, Some("^3.0.0")),
                ("5.4.0", false, Some("^3.0.0")),
            ],
        );

        // Case 1: `>=6.0.0` skips 6.1.0 & 6.0.0, STILL surfaces minor update 5.9.0
        let mut from_6 = PackageIgnoreRules {
            rules: Vec::new(),
            from_file: true,
            from_settings: false,
        };
        from_6.insert_normalized(IgnoreRule::parse(">=6.0.0").unwrap());

        let dio_eval_54 = DependencyEvaluation::evaluate(
            dio_dep.clone(),
            dio_info.clone(),
            Some("5.4.0".to_string()),
            parsed.environment_sdk.as_ref(),
            &config,
            from_6.clone(),
        );
        assert_eq!(
            dio_eval_54.latest_overall.as_ref().map(ToString::to_string),
            Some("5.9.0".to_string())
        );
        assert_eq!(
            dio_eval_54
                .raw_latest_overall
                .as_ref()
                .map(ToString::to_string),
            Some("6.1.0".to_string())
        );
        assert_eq!(dio_eval_54.upgrade_kind, UpgradeKind::Minor);
        assert!(dio_eval_54.muted_newer_version);

        // Hover reveals both unignored latest (5.9.0) and real pub.dev latest (6.1.0 ignored by >=6.0.0)
        let hover_54 = build_hosted_hover_markdown(&dio_eval_54, None, &config);
        assert!(hover_54.contains("Latest (unignored)**: `5.9.0`"));
        assert!(hover_54.contains("Latest on pub.dev**: `6.1.0` *(ignored by `>=6.0.0`)*"));
        assert!(hover_54.contains("Ignored Rules**: `>=6.0.0`"));

        // Case 2: Once declared & locked at 5.9.0 with `>=6.0.0`, diagnostics are empty AND InlayHint is completely silent (None)!
        let parsed_59 = parse_pubspec(
            "name: demo\nenvironment:\n  sdk: ^3.5.0\ndependencies:\n  dio: ^5.9.0\n",
        );
        let dio_dep_59 = parsed_59.dependencies[0].clone();
        let dio_eval_59 = DependencyEvaluation::evaluate(
            dio_dep_59,
            dio_info.clone(),
            Some("5.9.0".to_string()),
            parsed_59.environment_sdk.as_ref(),
            &config,
            from_6.clone(),
        );
        assert!(!dio_eval_59.has_update());
        assert!(dio_eval_59.muted_newer_version);
        assert!(
            build_pubspec_diagnostics(
                &parsed_59,
                std::slice::from_ref(&dio_eval_59),
                None,
                &config
            )
            .is_empty()
        );
        assert!(
            build_pubspec_inlay_hints(
                std::slice::from_ref(&dio_eval_59),
                Range::new(Position::new(0, 0), Position::new(10, 0)),
                &config,
            )
            .is_empty()
        );

        // Case 3: Exact `=6.1.0` ignores 6.1.0 only; if 6.2.0 is released, it prompts immediately
        let mut exact_61 = PackageIgnoreRules {
            rules: Vec::new(),
            from_file: true,
            from_settings: false,
        };
        exact_61.insert_normalized(IgnoreRule::parse("=6.1.0").unwrap());
        let dio_info_62 = sample_pkg(
            "dio",
            &[
                ("6.2.0", false, Some("^3.0.0")),
                ("6.1.0", false, Some("^3.0.0")),
                ("5.9.0", false, Some("^3.0.0")),
            ],
        );
        let dio_eval_62 = DependencyEvaluation::evaluate(
            dio_dep,
            dio_info_62,
            Some("5.9.0".to_string()),
            parsed.environment_sdk.as_ref(),
            &config,
            exact_61,
        );
        assert_eq!(
            dio_eval_62.latest_overall.as_ref().map(ToString::to_string),
            Some("6.2.0".to_string())
        );
        assert_eq!(dio_eval_62.upgrade_kind, UpgradeKind::Major);

        // Case 4: `>=2.0.0` still preserves `discontinued` warning, whereas `*` silences everything (incl. `discontinued`)
        let mut legacy_info = sample_pkg("legacy", &[("2.0.0", false, Some("^3.0.0")), ("1.0.0", false, Some("^3.0.0"))]);
        legacy_info.is_discontinued = true;
        let mut from_2 = PackageIgnoreRules {
            rules: Vec::new(),
            from_file: true,
            from_settings: false,
        };
        from_2.insert_normalized(IgnoreRule::parse(">=2.0.0").unwrap());
        let legacy_partial = DependencyEvaluation::evaluate(
            legacy_dep.clone(),
            legacy_info.clone(),
            Some("1.0.0".to_string()),
            parsed.environment_sdk.as_ref(),
            &config,
            from_2,
        );
        let partial_diags = build_pubspec_diagnostics(&parsed, &[legacy_partial], None, &config);
        assert!(partial_diags.iter().any(|d| d.code == Some(NumberOrString::String("discontinued".to_string()))));

        let mut ignore_all = PackageIgnoreRules {
            rules: Vec::new(),
            from_file: true,
            from_settings: false,
        };
        ignore_all.insert_normalized(IgnoreRule::All);
        let legacy_all = DependencyEvaluation::evaluate(
            legacy_dep,
            legacy_info,
            Some("1.0.0".to_string()),
            parsed.environment_sdk.as_ref(),
            &config,
            ignore_all.clone(),
        );
        assert!(
            build_pubspec_diagnostics(&parsed, std::slice::from_ref(&legacy_all), None, &config)
                .is_empty()
        );
        assert!(
            build_pubspec_inlay_hints(
                std::slice::from_ref(&legacy_all),
                Range::new(Position::new(0, 0), Position::new(10, 0)),
                &config,
            )
            .is_empty()
        );

        // Case 5: CodeActions offer Unignore + Open .zed/pubspec-ignore.json on ignored lines
        let uri: Uri = "file:///tmp/demo/pubspec.yaml".parse().expect("uri");
        let mut ignore_cfg = PubspecIgnoreConfig {
            has_ignore_file: true,
            ..Default::default()
        };
        ignore_cfg.packages.insert("legacy".to_string(), ignore_all);
        let actions = build_pubspec_code_actions(
            &uri,
            &parsed,
            std::slice::from_ref(&legacy_all),
            &ignore_cfg,
            Range::new(Position::new(5, 2), Position::new(5, 8)),
        );
        let titles: Vec<&str> = actions
            .iter()
            .filter_map(|a| match a {
                CodeActionOrCommand::CodeAction(ca) => Some(ca.title.as_str()),
                _ => None,
            })
            .collect();
        assert!(titles.iter().any(|t| t.contains("Unignore `legacy`")));
        assert!(titles.iter().any(|t| t.contains("Open .zed/pubspec-ignore.json")));

        // Case 6: Version completion marks ignored versions and preselects the first unignored stable version
        let yaml_typing = "name: demo\ndependencies:\n  dio: ^\n";
        let parsed_typing = parse_pubspec(yaml_typing);
        let ctx = completion_context_at(yaml_typing, &parsed_typing, Position::new(2, 8)).unwrap();
        let items = build_version_completion_items(
            &ctx,
            &dio_info,
            Some("5.4.0"),
            parsed.environment_sdk.as_ref(),
            &config,
            &from_6,
        );
        assert_eq!(items[0].label, "^6.1.0");
        assert!(items[0].detail.as_deref().unwrap_or("").contains("ignored"));
        assert_eq!(items[0].preselect, Some(false));
        assert_eq!(items[2].label, "^5.9.0");
        assert_eq!(items[2].preselect, Some(true));
    }

    #[test]
    fn handles_lockfile_ahead_drift_without_emojis_and_fixes_changelog_range() {
        use crate::lsp::pubspec::ignore::IgnoreRule;

        let yaml = concat!(
            "name: demo\n",
            "environment:\n",
            "  sdk: ^3.5.0\n",
            "dependencies:\n",
            "  dio: ^5.4.0\n",
            "  http: ^1.2.0\n",
            "  intl: '>=0.18.0 <0.21.0'\n",
        );
        let parsed = parse_pubspec(yaml);
        let config = PubspecConfig::default();

        let dio_dep = parsed.dependencies[0].clone();
        let http_dep = parsed.dependencies[1].clone();
        let intl_dep = parsed.dependencies[2].clone();

        let dio_info = sample_pkg(
            "dio",
            &[
                ("5.9.0", false, Some("^3.0.0")),
                ("5.7.0", false, Some("^3.0.0")),
                ("5.4.0", false, Some("^3.0.0")),
            ],
        );
        let http_info = sample_pkg(
            "http",
            &[
                ("2.0.0", false, Some("^3.0.0")),
                ("1.5.0", false, Some("^3.0.0")),
                ("1.2.0", false, Some("^3.0.0")),
            ],
        );
        let intl_info = sample_pkg(
            "intl",
            &[
                ("0.20.0", false, Some("^3.0.0")),
                ("0.18.0", false, Some("^3.0.0")),
            ],
        );

        // dio: YAML ^5.4.0, lock 5.9.0 (already latest on pub.dev) -> pure lock drift
        let dio_eval = DependencyEvaluation::evaluate(
            dio_dep,
            dio_info,
            Some("5.9.0".to_string()),
            parsed.environment_sdk.as_ref(),
            &config,
            PackageIgnoreRules::default(),
        );
        // http: YAML ^1.2.0, lock 1.5.0, pub.dev 2.0.0 -> lock drift + major update
        let http_eval = DependencyEvaluation::evaluate(
            http_dep,
            http_info,
            Some("1.5.0".to_string()),
            parsed.environment_sdk.as_ref(),
            &config,
            PackageIgnoreRules::default(),
        );
        // intl: YAML '>=0.18.0 <0.21.0', lock 0.20.0 (latest) -> compound range tightening
        let intl_eval = DependencyEvaluation::evaluate(
            intl_dep,
            intl_info,
            Some("0.20.0".to_string()),
            parsed.environment_sdk.as_ref(),
            &config,
            PackageIgnoreRules::default(),
        );

        let evals = vec![dio_eval.clone(), http_eval.clone(), intl_eval.clone()];

        // 1. Diagnostics: dio & intl emit INFO `lock-drift`, http emits WARNING `outdated-major` mentioning YAML constraint
        let diags = build_pubspec_diagnostics(&parsed, &evals, None, &config);
        assert_eq!(diags.len(), 3);
        let dio_diag = diags
            .iter()
            .find(|d| d.range.start.line == 4)
            .expect("dio diag");
        assert_eq!(
            dio_diag.code,
            Some(NumberOrString::String("lock-drift".to_string()))
        );
        assert_eq!(dio_diag.severity, Some(DiagnosticSeverity::INFORMATION));
        assert!(dio_diag.message.contains("Locked at 5.9.0"));
        assert!(dio_diag.message.contains("^5.9.0"));

        let http_diag = diags
            .iter()
            .find(|d| d.range.start.line == 5)
            .expect("http diag");
        assert_eq!(
            http_diag.code,
            Some(NumberOrString::String("outdated-major".to_string()))
        );
        assert!(http_diag.message.contains("pubspec.yaml declares ^1.2.0"));

        // 2. Inlay hints: crisp monochrome labels, ZERO emoji characters, clickable sync edit
        let hints = build_pubspec_inlay_hints(
            &evals,
            Range::new(Position::new(0, 0), Position::new(10, 0)),
            &config,
        );
        assert_eq!(hints.len(), 3);
        let hint_labels: Vec<String> = hints
            .iter()
            .map(|h| match &h.label {
                InlayHintLabel::String(s) => s.clone(),
                InlayHintLabel::LabelParts(_) => String::new(),
            })
            .collect();
        assert_eq!(hint_labels[0], "⇄ 5.9.0 (locked)");
        assert_eq!(hint_labels[1], "locked 1.5.0 · ⬆ 2.0.0 (major)");
        assert_eq!(hint_labels[2], "⇄ 0.20.0 (locked)");
        // Ensure no color emojis (e.g. U+1F300..U+1FAFF) ever sneak into hint labels
        assert!(
            hint_labels
                .iter()
                .all(|s| !s.chars().any(|c| ('\u{1F300}'..='\u{1FAFF}').contains(&c)))
        );
        assert_eq!(
            hints[0].text_edits.as_ref().expect("dio edit")[0].new_text,
            "^5.9.0"
        );
        assert_eq!(
            hints[2].text_edits.as_ref().expect("intl edit")[0].new_text,
            "'>=0.20.0 <0.21.0'"
        );

        // 3. CodeActions on dio line: preferred single sync + batch sync for all 3 drifted packages
        let uri: Uri = "file:///tmp/demo/pubspec.yaml".parse().expect("uri");
        let empty_ignore = PubspecIgnoreConfig::default();
        let dio_actions = build_pubspec_code_actions(
            &uri,
            &parsed,
            &evals,
            &empty_ignore,
            Range::new(Position::new(4, 2), Position::new(4, 6)),
        );
        let sync_dio = dio_actions.iter().find_map(|a| match a {
            CodeActionOrCommand::CodeAction(ca)
                if ca.title == "Sync `dio` to ^5.9.0 (from pubspec.lock)" =>
            {
                Some(ca)
            }
            _ => None,
        });
        assert_eq!(sync_dio.expect("sync dio").is_preferred, Some(true));
        assert!(dio_actions.iter().any(|a| match a {
            CodeActionOrCommand::CodeAction(ca) => {
                ca.title == "Flutter Debugger: Sync all dependencies to pubspec.lock (3 packages)"
            }
            _ => false,
        }));

        // On http line: offers BOTH major upgrade to ^2.0.0 AND sync to ^1.5.0
        let http_actions = build_pubspec_code_actions(
            &uri,
            &parsed,
            &evals,
            &empty_ignore,
            Range::new(Position::new(5, 2), Position::new(5, 6)),
        );
        assert!(http_actions.iter().any(|a| match a {
            CodeActionOrCommand::CodeAction(ca) => ca.title.contains("Upgrade `http` to ^2.0.0"),
            _ => false,
        }));
        assert!(http_actions.iter().any(|a| match a {
            CodeActionOrCommand::CodeAction(ca) => {
                ca.title == "Sync `http` to ^1.5.0 (from pubspec.lock)"
            }
            _ => false,
        }));

        // 4. Hover Changelog uses `declared_version` (5.4.0) -> `5.9.0`, showing ALL intermediate releases
        let changelog = vec![
            ChangelogEntry {
                version: "5.9.0".to_string(),
                heading: "5.9.0".to_string(),
                body_markdown: "- Added HTTP/3 support".to_string(),
            },
            ChangelogEntry {
                version: "5.7.0".to_string(),
                heading: "5.7.0".to_string(),
                body_markdown: "- Improved cancellation tokens".to_string(),
            },
            ChangelogEntry {
                version: "5.4.0".to_string(),
                heading: "5.4.0".to_string(),
                body_markdown: "- Baseline release".to_string(),
            },
        ];
        let hover_md = build_hosted_hover_markdown(&dio_eval, Some(&changelog), &config);
        assert!(hover_md.contains("Out of sync with `pubspec.lock`"));
        assert!(hover_md.contains("Added HTTP/3 support"));
        assert!(hover_md.contains("Improved cancellation tokens"));
        assert!(!hover_md.contains("Baseline release"));

        // 5. CodeLens reports updates + to sync count, and reflects `pub_upgrade_tighten`
        let lenses = build_pubspec_code_lenses(
            &uri,
            &parsed,
            ProjectKind::Flutter,
            &evals,
            false,
            None,
            &config,
        );
        assert_eq!(
            lenses[0].command.as_ref().expect("c").title,
            "Refresh (1 update, 2 to sync)"
        );
        assert_eq!(
            lenses[2].command.as_ref().expect("c").title,
            "flutter pub upgrade"
        );

        let mut tighten_cfg = config.clone();
        tighten_cfg.pub_upgrade_tighten = true;
        let lenses_tighten = build_pubspec_code_lenses(
            &uri,
            &parsed,
            ProjectKind::Flutter,
            std::slice::from_ref(&dio_eval),
            false,
            None,
            &tighten_cfg,
        );
        assert_eq!(
            lenses_tighten[0].command.as_ref().expect("c").title,
            "Refresh (1 to sync with lock)"
        );
        assert_eq!(
            lenses_tighten[2].command.as_ref().expect("c").title,
            "flutter pub upgrade --tighten"
        );

        // 6. When `>=2.0.0` ignores http's major update while lock 1.5.0 > YAML ^1.2.0,
        // lock-drift sync still surfaces until synced; once synced to ^1.5.0, line goes completely silent.
        let mut ignore_2 = PackageIgnoreRules {
            rules: Vec::new(),
            from_file: true,
            from_settings: false,
        };
        ignore_2.insert_normalized(IgnoreRule::parse(">=2.0.0").unwrap());
        let http_drifted_muted = DependencyEvaluation::evaluate(
            parsed.dependencies[1].clone(),
            sample_pkg("http", &[("2.0.0", false, Some("^3.0.0")), ("1.5.0", false, Some("^3.0.0"))]),
            Some("1.5.0".to_string()),
            parsed.environment_sdk.as_ref(),
            &config,
            ignore_2.clone(),
        );
        assert_eq!(
            build_pubspec_inlay_hints(
                std::slice::from_ref(&http_drifted_muted),
                Range::new(Position::new(0, 0), Position::new(10, 0)),
                &config,
            )
            .len(),
            1
        );

        let synced_yaml = "name: demo\ndependencies:\n  http: ^1.5.0\n";
        let synced_parsed = parse_pubspec(synced_yaml);
        let http_synced_muted = DependencyEvaluation::evaluate(
            synced_parsed.dependencies[0].clone(),
            sample_pkg("http", &[("2.0.0", false, Some("^3.0.0")), ("1.5.0", false, Some("^3.0.0"))]),
            Some("1.5.0".to_string()),
            None,
            &config,
            ignore_2,
        );
        assert!(
            build_pubspec_inlay_hints(
                std::slice::from_ref(&http_synced_muted),
                Range::new(Position::new(0, 0), Position::new(10, 0)),
                &config,
            )
            .is_empty()
        );
    }
}
