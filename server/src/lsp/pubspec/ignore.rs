use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::debug_config::strip_jsonc;
use crate::lsp::pubspec::version::{compare_dart_versions, parse_dart_version};
use crate::project::find_project_for_path;

pub const IGNORE_FILE_REL_PATH: &str = ".zed/pubspec-ignore.json";

static IGNORE_TMP_COUNTER: AtomicU64 = AtomicU64::new(1);

/// A single version/package ignore rule stored in `.zed/pubspec-ignore.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgnoreRule {
    /// Ignore only one specific version (e.g. `=5.9.0`). Newer versions will still prompt.
    Exact(Version),
    /// Ignore this version and all newer versions (e.g. `>=6.0.0`). Lower un-ignored updates still prompt.
    FromVersion(Version),
    /// Ignore all updates (and discontinued/retracted diagnostics) for this package (`*`).
    All,
}

impl IgnoreRule {
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        if trimmed == "*" || trimmed.eq_ignore_ascii_case("all") {
            return Some(Self::All);
        }
        if let Some(rest) = trimmed.strip_prefix(">=") {
            return parse_dart_version(rest.trim()).map(Self::FromVersion);
        }
        if let Some(rest) = trimmed
            .strip_prefix("==")
            .or_else(|| trimmed.strip_prefix('='))
        {
            return parse_dart_version(rest.trim()).map(Self::Exact);
        }
        parse_dart_version(trimmed).map(Self::Exact)
    }

    #[must_use]
    pub fn to_canonical_string(&self) -> String {
        match self {
            Self::All => "*".to_string(),
            Self::FromVersion(v) => format!(">={v}"),
            Self::Exact(v) => format!("={v}"),
        }
    }

    #[must_use]
    pub fn matches_version(&self, candidate: &Version) -> bool {
        match self {
            Self::All => true,
            Self::Exact(v) => compare_dart_versions(candidate, v) == Ordering::Equal,
            Self::FromVersion(v) => compare_dart_versions(candidate, v) != Ordering::Less,
        }
    }

    fn sort_cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::All, Self::All) => Ordering::Equal,
            (Self::All, _) => Ordering::Less,
            (_, Self::All) => Ordering::Greater,
            (Self::FromVersion(a), Self::FromVersion(b)) => compare_dart_versions(a, b),
            (Self::FromVersion(_), Self::Exact(_)) => Ordering::Less,
            (Self::Exact(_), Self::FromVersion(_)) => Ordering::Greater,
            (Self::Exact(a), Self::Exact(b)) => compare_dart_versions(a, b),
        }
    }
}

/// Effective normalized ignore rules for a single package across `.zed/pubspec-ignore.json`
/// and `settings.json` `pubspec.ignorePackages`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageIgnoreRules {
    pub rules: Vec<IgnoreRule>,
    pub from_file: bool,
    pub from_settings: bool,
}

impl PackageIgnoreRules {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    #[must_use]
    pub fn is_all_ignored(&self) -> bool {
        self.rules.iter().any(|r| matches!(r, IgnoreRule::All))
    }

    #[must_use]
    pub fn matches_version(&self, candidate: &Version) -> bool {
        self.rules.iter().any(|r| r.matches_version(candidate))
    }

    #[must_use]
    pub fn matching_rule_display(&self, candidate: &Version) -> Option<String> {
        self.rules
            .iter()
            .find(|r| r.matches_version(candidate))
            .map(IgnoreRule::to_canonical_string)
    }

    #[must_use]
    pub fn summary(&self) -> String {
        self.rules
            .iter()
            .map(IgnoreRule::to_canonical_string)
            .collect::<Vec<_>>()
            .join(", ")
    }

    #[must_use]
    pub fn source_label(&self) -> &'static str {
        match (self.from_file, self.from_settings) {
            (true, true) => ".zed/pubspec-ignore.json & settings.json",
            (true, false) => ".zed/pubspec-ignore.json",
            (false, true) => "settings.json (pubspec.ignorePackages)",
            (false, false) => "",
        }
    }

    /// Inserts a rule while deduplicating and absorbing redundant narrower rules.
    pub fn insert_normalized(&mut self, rule: IgnoreRule) {
        if self.is_all_ignored() {
            return;
        }
        match rule {
            IgnoreRule::All => {
                self.rules.clear();
                self.rules.push(IgnoreRule::All);
            }
            IgnoreRule::FromVersion(v) => {
                let already_broader = self.rules.iter().any(|existing| {
                    matches!(existing, IgnoreRule::FromVersion(u) if compare_dart_versions(u, &v) != Ordering::Greater)
                });
                if already_broader {
                    return;
                }
                self.rules.retain(|existing| match existing {
                    IgnoreRule::All => true,
                    IgnoreRule::FromVersion(w) | IgnoreRule::Exact(w) => {
                        compare_dart_versions(w, &v) == Ordering::Less
                    }
                });
                self.rules.push(IgnoreRule::FromVersion(v));
                self.rules.sort_by(IgnoreRule::sort_cmp);
            }
            IgnoreRule::Exact(v) => {
                if self.matches_version(&v) {
                    return;
                }
                self.rules.push(IgnoreRule::Exact(v));
                self.rules.sort_by(IgnoreRule::sort_cmp);
            }
        }
    }
}

/// Merged ignore configuration active for an open `pubspec.yaml` / `pubspec_overrides.yaml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PubspecIgnoreConfig {
    pub packages: BTreeMap<String, PackageIgnoreRules>,
    pub has_ignore_file: bool,
}

impl PubspecIgnoreConfig {
    #[must_use]
    pub fn for_package(&self, name: &str) -> PackageIgnoreRules {
        self.packages.get(name).cloned().unwrap_or_default()
    }

    #[must_use]
    pub fn is_all_ignored(&self, name: &str) -> bool {
        self.packages
            .get(name)
            .is_some_and(PackageIgnoreRules::is_all_ignored)
    }

    #[must_use]
    pub fn has_file_rule(&self, name: &str) -> bool {
        self.packages
            .get(name)
            .is_some_and(|r| r.from_file && !r.is_empty())
    }

    #[must_use]
    pub fn has_any_rules(&self) -> bool {
        !self.packages.is_empty()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RawRuleValue {
    Single(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawIgnoreFile {
    #[serde(default)]
    packages: BTreeMap<String, RawRuleValue>,
}

#[derive(Debug, Serialize)]
struct CanonicalIgnoreFile {
    packages: BTreeMap<String, Vec<String>>,
}

/// Resolves the Zed workspace root containing `doc_path` (preferring the deepest matching root),
/// falling back to the nearest Dart/Flutter project root or parent directory.
#[must_use]
pub fn resolve_workspace_root_for_path(
    doc_path: &Path,
    workspace_roots: &[PathBuf],
) -> Option<PathBuf> {
    let canonical_doc = fs::canonicalize(doc_path).ok();

    let mut best: Option<(usize, PathBuf)> = None;
    for root in workspace_roots {
        let direct_match = doc_path.starts_with(root);
        let canonical_match = !direct_match
            && canonical_doc.as_ref().is_some_and(|cd| {
                fs::canonicalize(root)
                    .ok()
                    .is_some_and(|cr| cd.starts_with(&cr))
            });
        if direct_match || canonical_match {
            let depth = root.components().count();
            if best.as_ref().is_none_or(|(best_depth, _)| depth > *best_depth) {
                best = Some((depth, root.clone()));
            }
        }
    }

    best.map(|(_, r)| r)
        .or_else(|| find_project_for_path(doc_path).map(|p| p.root))
        .or_else(|| doc_path.parent().map(Path::to_path_buf))
}

/// Returns the primary `.zed/pubspec-ignore.json` path (at the Zed workspace root,
/// which equals the project root in single-package projects).
#[must_use]
pub fn resolve_primary_ignore_file(
    doc_path: &Path,
    workspace_roots: &[PathBuf],
) -> Option<PathBuf> {
    resolve_workspace_root_for_path(doc_path, workspace_roots)
        .map(|root| root.join(IGNORE_FILE_REL_PATH))
}

/// Returns all applicable `.zed/pubspec-ignore.json` files for `doc_path`:
/// the workspace root file plus (in a monorepo) the subpackage's own file if distinct.
#[must_use]
pub fn resolve_all_ignore_files(doc_path: &Path, workspace_roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(primary) = resolve_primary_ignore_file(doc_path, workspace_roots) {
        files.push(primary);
    }
    if let Some(project) = find_project_for_path(doc_path) {
        let sub_file = project.root.join(IGNORE_FILE_REL_PATH);
        let already_listed = files.iter().any(|existing| same_path(existing, &sub_file));
        if !already_listed {
            files.push(sub_file);
        }
    }
    files
}

fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => match (
            a.parent().and_then(|p| fs::canonicalize(p).ok()),
            b.parent().and_then(|p| fs::canonicalize(p).ok()),
        ) {
            (Some(pa), Some(pb)) => pa == pb && a.file_name() == b.file_name(),
            _ => false,
        },
    }
}

/// Loads and merges all effective ignore rules for `doc_path` from:
/// 1. `settings.json` `pubspec.ignorePackages` (mapped to `*`),
/// 2. `<workspace_root>/.zed/pubspec-ignore.json`,
/// 3. `<project_root>/.zed/pubspec-ignore.json` (when in a monorepo subpackage).
#[must_use]
pub fn load_effective_ignore_config(
    doc_path: &Path,
    workspace_roots: &[PathBuf],
    settings_ignore_packages: &[String],
) -> PubspecIgnoreConfig {
    let mut cfg = PubspecIgnoreConfig::default();

    for pkg in settings_ignore_packages {
        let trimmed = pkg.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry = cfg.packages.entry(trimmed.to_string()).or_default();
        entry.from_settings = true;
        entry.insert_normalized(IgnoreRule::All);
    }

    for file_path in resolve_all_ignore_files(doc_path, workspace_roots) {
        if !file_path.is_file() {
            continue;
        }
        cfg.has_ignore_file = true;
        match read_ignore_file_packages(&file_path) {
            Ok(file_packages) => {
                for (pkg, pkg_rules) in file_packages {
                    if pkg_rules.is_empty() {
                        continue;
                    }
                    let entry = cfg.packages.entry(pkg).or_default();
                    entry.from_file = true;
                    for rule in pkg_rules.rules {
                        entry.insert_normalized(rule);
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    path = %file_path.display(),
                    %err,
                    "ignoring malformed .zed/pubspec-ignore.json"
                );
            }
        }
    }

    cfg
}

fn read_ignore_file_packages(
    path: &Path,
) -> Result<BTreeMap<String, PackageIgnoreRules>, String> {
    if !path.is_file() {
        return Ok(BTreeMap::new());
    }
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read `{}`: {e}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    let cleaned = strip_jsonc(&raw);
    let parsed: RawIgnoreFile = serde_json::from_str(&cleaned).map_err(|e| {
        format!(
            "Refusing to overwrite `{}` because it is not valid JSON/JSONC ({e}). Please fix syntax manually.",
            path.display()
        )
    })?;

    let mut out: BTreeMap<String, PackageIgnoreRules> = BTreeMap::new();
    for (pkg, raw_val) in parsed.packages {
        let pkg_trimmed = pkg.trim();
        if pkg_trimmed.is_empty() {
            continue;
        }
        let raw_list: Vec<String> = match raw_val {
            RawRuleValue::Single(s) => vec![s],
            RawRuleValue::Many(v) => v,
        };
        let mut rules = PackageIgnoreRules {
            rules: Vec::new(),
            from_file: true,
            from_settings: false,
        };
        for item in raw_list {
            if let Some(rule) = IgnoreRule::parse(&item) {
                rules.insert_normalized(rule);
            }
        }
        if !rules.is_empty() {
            out.insert(pkg_trimmed.to_string(), rules);
        }
    }
    Ok(out)
}

fn write_ignore_file_atomic(
    path: &Path,
    packages: &BTreeMap<String, PackageIgnoreRules>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create `{}`: {e}", parent.display()))?;
    }

    let canonical = CanonicalIgnoreFile {
        packages: packages
            .iter()
            .filter(|(_, r)| !r.is_empty())
            .map(|(pkg, r)| {
                (
                    pkg.clone(),
                    r.rules
                        .iter()
                        .map(IgnoreRule::to_canonical_string)
                        .collect(),
                )
            })
            .collect(),
    };

    let mut content = serde_json::to_string_pretty(&canonical)
        .map_err(|e| format!("Failed to serialize ignore config: {e}"))?;
    content.push('\n');

    let seq = IGNORE_TMP_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    let tmp = path.with_extension(format!("json.{}.{seq}.tmp", std::process::id()));
    fs::write(&tmp, content.as_bytes())
        .map_err(|e| format!("Failed to write temp file `{}`: {e}", tmp.display()))?;

    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("Failed to update `{}`: {err}", path.display()));
    }

    Ok(())
}

/// Atomically adds and normalizes an ignore rule for `package` in `path`.
pub fn add_ignore_rule(path: &Path, package: &str, rule: IgnoreRule) -> Result<(), String> {
    let mut packages = read_ignore_file_packages(path)?;
    let entry = packages.entry(package.to_string()).or_default();
    entry.from_file = true;
    entry.insert_normalized(rule);
    write_ignore_file_atomic(path, &packages)
}

/// Removes all ignore rules for `package` across all applicable ignore files (workspace + subpackage).
/// Returns `Ok(true)` if at least one file was updated.
pub fn remove_package_ignore_rules(paths: &[PathBuf], package: &str) -> Result<bool, String> {
    let mut removed_any = false;
    for path in paths {
        if !path.is_file() {
            continue;
        }
        let mut packages = read_ignore_file_packages(path)?;
        if packages.remove(package).is_some() {
            write_ignore_file_atomic(path, &packages)?;
            removed_any = true;
        }
    }
    Ok(removed_any)
}

/// Creates `.zed/pubspec-ignore.json` with an empty skeleton if it does not yet exist.
pub fn ensure_ignore_file_exists(path: &Path) -> Result<(), String> {
    if path.is_file() {
        return Ok(());
    }
    write_ignore_file_atomic(path, &BTreeMap::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_matches_and_normalizes_ignore_rules() {
        let exact = IgnoreRule::parse("=5.9.0").expect("exact");
        let bare = IgnoreRule::parse("5.9.0").expect("bare");
        let from_v = IgnoreRule::parse(">=6.0.0").expect("from");
        let all = IgnoreRule::parse("*").expect("all");

        assert_eq!(exact, bare);
        assert_eq!(exact.to_canonical_string(), "=5.9.0");
        assert_eq!(from_v.to_canonical_string(), ">=6.0.0");
        assert_eq!(all.to_canonical_string(), "*");

        let v58 = parse_dart_version("5.8.0").unwrap();
        let v59 = parse_dart_version("5.9.0").unwrap();
        let v60_pre = parse_dart_version("6.0.0-dev.1").unwrap();
        let v60 = parse_dart_version("6.0.0").unwrap();
        let v61 = parse_dart_version("6.1.0").unwrap();

        assert!(exact.matches_version(&v59));
        assert!(!exact.matches_version(&v58));
        assert!(!exact.matches_version(&v60));

        assert!(!from_v.matches_version(&v59));
        assert!(!from_v.matches_version(&v60_pre));
        assert!(from_v.matches_version(&v60));
        assert!(from_v.matches_version(&v61));

        // Normalization: >=6.0.0 absorbs =6.1.0 and >=6.2.0 while preserving =5.9.0
        let mut rules = PackageIgnoreRules::default();
        rules.insert_normalized(IgnoreRule::parse("=5.9.0").unwrap());
        rules.insert_normalized(IgnoreRule::parse("=6.1.0").unwrap());
        rules.insert_normalized(IgnoreRule::parse(">=6.2.0").unwrap());
        rules.insert_normalized(IgnoreRule::parse(">=6.0.0").unwrap());
        assert_eq!(rules.summary(), ">=6.0.0, =5.9.0");

        // Adding narrower >=6.5.0 or =6.3.0 is a no-op
        rules.insert_normalized(IgnoreRule::parse(">=6.5.0").unwrap());
        rules.insert_normalized(IgnoreRule::parse("=6.3.0").unwrap());
        assert_eq!(rules.summary(), ">=6.0.0, =5.9.0");

        // Adding * collapses everything into *
        rules.insert_normalized(IgnoreRule::All);
        assert_eq!(rules.summary(), "*");
    }

    #[test]
    fn persists_atomically_refuses_corrupt_json_and_merges_monorepo_sources() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_root = dir.path().to_path_buf();
        let sub_pkg = workspace_root.join("apps/mobile");
        fs::create_dir_all(&sub_pkg).expect("mkdir sub");
        let pubspec_path = sub_pkg.join("pubspec.yaml");
        fs::write(&pubspec_path, "name: mobile\n").expect("write pubspec");

        let ws_ignore = resolve_primary_ignore_file(
            &pubspec_path,
            std::slice::from_ref(&workspace_root),
        )
        .expect("primary");
        assert_eq!(ws_ignore, workspace_root.join(IGNORE_FILE_REL_PATH));

        // Write rules via API
        add_ignore_rule(&ws_ignore, "dio", IgnoreRule::parse("=5.9.0").unwrap()).expect("add dio");
        add_ignore_rule(&ws_ignore, "http", IgnoreRule::parse(">=2.0.0").unwrap())
            .expect("add http");

        // Hand-written JSONC with comments + shorthand string in subpackage
        let sub_ignore = sub_pkg.join(IGNORE_FILE_REL_PATH);
        fs::create_dir_all(sub_ignore.parent().unwrap()).expect("mkdir sub .zed");
        fs::write(
            &sub_ignore,
            r#"{
              // monorepo subpackage override
              "packages": {
                "dio": ">=6.0.0",
                "bloc": "*",
              },
            }"#,
        )
        .expect("write sub ignore");

        let merged = load_effective_ignore_config(
            &pubspec_path,
            std::slice::from_ref(&workspace_root),
            &["settings_pkg".to_string()],
        );
        assert!(merged.has_ignore_file);
        assert_eq!(merged.for_package("dio").summary(), ">=6.0.0, =5.9.0");
        assert_eq!(merged.for_package("http").summary(), ">=2.0.0");
        assert!(merged.is_all_ignored("bloc"));
        assert!(merged.is_all_ignored("settings_pkg"));
        assert!(merged.for_package("settings_pkg").from_settings);
        assert!(!merged.for_package("settings_pkg").from_file);

        // Unignore removes from BOTH workspace and subpackage files
        let all_files =
            resolve_all_ignore_files(&pubspec_path, std::slice::from_ref(&workspace_root));
        assert!(remove_package_ignore_rules(&all_files, "dio").expect("remove dio"));
        let after = load_effective_ignore_config(
            &pubspec_path,
            std::slice::from_ref(&workspace_root),
            &[],
        );
        assert!(after.for_package("dio").is_empty());

        // Refuses to overwrite broken JSON
        let broken = "{ \"packages\": { \"dio\": [ ";
        fs::write(&ws_ignore, broken).expect("write broken");
        assert!(add_ignore_rule(&ws_ignore, "dio", IgnoreRule::All).is_err());
        assert_eq!(fs::read_to_string(&ws_ignore).unwrap(), broken);
    }
}
