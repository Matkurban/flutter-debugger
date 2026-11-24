use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower_lsp_server::ls_types::{DiagnosticSeverity, Uri};

use crate::cache::DEFAULT_DEVICE_CACHE_TTL_SECS;
use crate::lsp::dart_file::{DartFileRole, ParsedDartFile, classify_dart_file_role, parse_dart_file};
use crate::lsp::pubspec::{
    DependencyEvaluation, ParsedPubspec, PubspecIgnoreConfig, parse_pubspec,
};
use crate::project::{ProjectInfo, find_project_for_path};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeverityLevel {
    Error,
    Warning,
    Info,
    Hint,
    Off,
}

impl SeverityLevel {
    #[must_use]
    pub fn to_lsp(self) -> Option<DiagnosticSeverity> {
        match self {
            Self::Error => Some(DiagnosticSeverity::ERROR),
            Self::Warning => Some(DiagnosticSeverity::WARNING),
            Self::Info => Some(DiagnosticSeverity::INFORMATION),
            Self::Hint => Some(DiagnosticSeverity::HINT),
            Self::Off => None,
        }
    }
}

/// Nested `pubspec` configuration under `lsp.flutter-debugger.initialization_options.pubspec`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PubspecConfig {
    pub enabled: bool,
    pub pub_hosted_url: Option<String>,
    pub max_concurrency: usize,
    pub request_timeout_secs: u64,
    pub cache_ttl_secs: u64,
    pub changelog_cache_ttl_secs: u64,
    pub include_prerelease: bool,
    pub inlay_hints: bool,
    pub show_up_to_date_hints: bool,
    pub code_lens: bool,
    pub code_lens_refresh: bool,
    pub code_lens_pub_get: bool,
    pub code_lens_pub_upgrade: bool,
    pub code_lens_pub_outdated: bool,
    pub check_dev_dependencies: bool,
    pub check_dependency_overrides: bool,
    pub auto_pub_get_on_save: bool,
    pub pub_upgrade_tighten: bool,
    pub max_changelog_versions: usize,
    pub major_severity: SeverityLevel,
    pub minor_patch_severity: SeverityLevel,
    pub lock_drift_severity: SeverityLevel,
    pub ignore_packages: Vec<String>,
}

impl Default for PubspecConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            pub_hosted_url: None,
            max_concurrency: 10,
            request_timeout_secs: 15,
            cache_ttl_secs: 24 * 60 * 60,
            changelog_cache_ttl_secs: 24 * 60 * 60,
            include_prerelease: false,
            inlay_hints: true,
            show_up_to_date_hints: true,
            code_lens: true,
            code_lens_refresh: true,
            code_lens_pub_get: true,
            code_lens_pub_upgrade: true,
            code_lens_pub_outdated: true,
            check_dev_dependencies: true,
            check_dependency_overrides: true,
            auto_pub_get_on_save: true,
            pub_upgrade_tighten: false,
            max_changelog_versions: 3,
            major_severity: SeverityLevel::Warning,
            minor_patch_severity: SeverityLevel::Info,
            lock_drift_severity: SeverityLevel::Info,
            ignore_packages: Vec::new(),
        }
    }
}

impl PubspecConfig {
    /// Resolves the active pub hosted URL following Dart's precedence:
    /// explicit `pubspec.pubHostedUrl` -> `PUB_HOSTED_URL` env var -> `https://pub.dev`.
    #[must_use]
    pub fn resolved_hosted_url(&self) -> String {
        let raw = self
            .pub_hosted_url
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                std::env::var("PUB_HOSTED_URL")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
            })
            .unwrap_or_else(|| "https://pub.dev".to_string());

        raw.trim_end_matches('/').to_string()
    }

    fn apply_patch(&mut self, patch: PubspecConfigPatch) -> bool {
        let mut changed = false;
        if let Some(v) = patch.enabled {
            self.enabled = v;
            changed = true;
        }
        if let Some(v) = patch.pub_hosted_url {
            let trimmed = v.trim();
            self.pub_hosted_url = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
            changed = true;
        }
        if let Some(v) = patch.max_concurrency {
            self.max_concurrency = v.clamp(1, 32);
            changed = true;
        }
        if let Some(v) = patch.request_timeout_secs {
            self.request_timeout_secs = v.max(3);
            changed = true;
        }
        if let Some(v) = patch.cache_ttl_secs {
            self.cache_ttl_secs = v;
            changed = true;
        }
        if let Some(v) = patch.changelog_cache_ttl_secs {
            self.changelog_cache_ttl_secs = v;
            changed = true;
        }
        if let Some(v) = patch.include_prerelease {
            self.include_prerelease = v;
            changed = true;
        }
        if let Some(v) = patch.inlay_hints {
            self.inlay_hints = v;
            changed = true;
        }
        if let Some(v) = patch.show_up_to_date_hints {
            self.show_up_to_date_hints = v;
            changed = true;
        }
        if let Some(v) = patch.code_lens {
            self.code_lens = v;
            changed = true;
        }
        if let Some(v) = patch.code_lens_refresh {
            self.code_lens_refresh = v;
            changed = true;
        }
        if let Some(v) = patch.code_lens_pub_get {
            self.code_lens_pub_get = v;
            changed = true;
        }
        if let Some(v) = patch.code_lens_pub_upgrade {
            self.code_lens_pub_upgrade = v;
            changed = true;
        }
        if let Some(v) = patch.code_lens_pub_outdated {
            self.code_lens_pub_outdated = v;
            changed = true;
        }
        if let Some(v) = patch.check_dev_dependencies {
            self.check_dev_dependencies = v;
            changed = true;
        }
        if let Some(v) = patch.check_dependency_overrides {
            self.check_dependency_overrides = v;
            changed = true;
        }
        if let Some(v) = patch.auto_pub_get_on_save {
            self.auto_pub_get_on_save = v;
            changed = true;
        }
        if let Some(v) = patch.pub_upgrade_tighten {
            self.pub_upgrade_tighten = v;
            changed = true;
        }
        if let Some(v) = patch.max_changelog_versions {
            self.max_changelog_versions = v;
            changed = true;
        }
        if let Some(v) = patch.major_severity {
            self.major_severity = v;
            changed = true;
        }
        if let Some(v) = patch.minor_patch_severity {
            self.minor_patch_severity = v;
            changed = true;
        }
        if let Some(v) = patch.lock_drift_severity {
            self.lock_drift_severity = v;
            changed = true;
        }
        if let Some(v) = patch.ignore_packages {
            self.ignore_packages = v;
            changed = true;
        }
        changed
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PubspecConfigPatch {
    pub enabled: Option<bool>,
    pub pub_hosted_url: Option<String>,
    pub max_concurrency: Option<usize>,
    pub request_timeout_secs: Option<u64>,
    pub cache_ttl_secs: Option<u64>,
    pub changelog_cache_ttl_secs: Option<u64>,
    pub include_prerelease: Option<bool>,
    pub inlay_hints: Option<bool>,
    pub show_up_to_date_hints: Option<bool>,
    pub code_lens: Option<bool>,
    pub code_lens_refresh: Option<bool>,
    pub code_lens_pub_get: Option<bool>,
    pub code_lens_pub_upgrade: Option<bool>,
    pub code_lens_pub_outdated: Option<bool>,
    pub check_dev_dependencies: Option<bool>,
    pub check_dependency_overrides: Option<bool>,
    pub auto_pub_get_on_save: Option<bool>,
    pub pub_upgrade_tighten: Option<bool>,
    pub max_changelog_versions: Option<usize>,
    pub major_severity: Option<SeverityLevel>,
    pub minor_patch_severity: Option<SeverityLevel>,
    pub lock_drift_severity: Option<SeverityLevel>,
    pub ignore_packages: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ServerConfig {
    pub auto_hot_reload_on_save: bool,
    pub hot_reload_debounce_ms: u64,
    pub closing_labels: bool,
    pub closing_labels_min_lines: u32,
    pub color_previews: bool,
    pub expose_global_code_actions: bool,
    pub flutter_sdk_path: Option<String>,
    pub device_cache_ttl_secs: u64,
    pub pubspec: PubspecConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigPatch {
    pub auto_hot_reload_on_save: Option<bool>,
    pub hot_reload_debounce_ms: Option<u64>,
    pub closing_labels: Option<bool>,
    pub closing_labels_min_lines: Option<u32>,
    pub color_previews: Option<bool>,
    pub expose_global_code_actions: Option<bool>,
    pub flutter_sdk_path: Option<String>,
    pub device_cache_ttl_secs: Option<u64>,
    pub pubspec: Option<PubspecConfigPatch>,
}

impl ServerConfig {
    /// Incrementally merges a JSON configuration object into `self`.
    /// Empty objects (`{}`) or `null` preserve all existing fields without resetting to defaults.
    pub fn apply_json_patch(&mut self, value: &Value) -> bool {
        let Some(obj) = value.as_object() else {
            return false;
        };
        if obj.is_empty() {
            return false;
        }
        let target = obj
            .get("flutter-debugger")
            .or_else(|| obj.get("flutterDebugger"))
            .filter(|v| v.as_object().is_some_and(|m| !m.is_empty()))
            .unwrap_or(value);

        let Ok(patch) = serde_json::from_value::<ServerConfigPatch>(target.clone()) else {
            return false;
        };

        let mut changed = false;
        if let Some(v) = patch.auto_hot_reload_on_save {
            self.auto_hot_reload_on_save = v;
            changed = true;
        }
        if let Some(v) = patch.hot_reload_debounce_ms {
            self.hot_reload_debounce_ms = v;
            changed = true;
        }
        if let Some(v) = patch.closing_labels {
            self.closing_labels = v;
            changed = true;
        }
        if let Some(v) = patch.closing_labels_min_lines {
            self.closing_labels_min_lines = v;
            changed = true;
        }
        if let Some(v) = patch.color_previews {
            self.color_previews = v;
            changed = true;
        }
        if let Some(v) = patch.expose_global_code_actions {
            self.expose_global_code_actions = v;
            changed = true;
        }
        if let Some(v) = patch.flutter_sdk_path {
            let trimmed = v.trim();
            self.flutter_sdk_path = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
            changed = true;
        }
        if let Some(v) = patch.device_cache_ttl_secs {
            self.device_cache_ttl_secs = v;
            changed = true;
        }
        if let Some(pubspec_patch) = patch.pubspec
            && self.pubspec.apply_patch(pubspec_patch)
        {
            changed = true;
        }
        changed
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            auto_hot_reload_on_save: true,
            hot_reload_debounce_ms: 250,
            closing_labels: true,
            closing_labels_min_lines: 2,
            color_previews: true,
            expose_global_code_actions: false,
            flutter_sdk_path: None,
            device_cache_ttl_secs: DEFAULT_DEVICE_CACHE_TTL_SECS,
            pubspec: PubspecConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    Pubspec,
    PubspecOverrides,
    Dart,
    Ignored,
}

#[derive(Debug, Clone)]
pub struct OpenDocument {
    pub kind: DocumentKind,
    pub text: String,
    pub project: Option<ProjectInfo>,
    pub pubspec: Option<ParsedPubspec>,
    pub locked_versions: std::collections::HashMap<String, String>,
    pub evaluations: Vec<DependencyEvaluation>,
    pub ignore_config: PubspecIgnoreConfig,
    pub generation: u64,
    pub dart: Option<ParsedDartFile>,
}

impl OpenDocument {
    pub fn parse_sync(uri: &Uri, text: &str) -> Option<Self> {
        let path = uri_to_file_path(uri)?;
        let kind = classify_document_path(&path);

        match kind {
            DocumentKind::Ignored => Some(Self {
                kind,
                text: String::new(),
                project: None,
                pubspec: None,
                locked_versions: std::collections::HashMap::new(),
                evaluations: Vec::new(),
                ignore_config: PubspecIgnoreConfig::default(),
                generation: 0,
                dart: None,
            }),
            DocumentKind::Pubspec | DocumentKind::PubspecOverrides => Some(Self {
                kind,
                text: text.to_string(),
                project: find_project_for_path(&path),
                pubspec: Some(parse_pubspec(text)),
                locked_versions: std::collections::HashMap::new(),
                evaluations: Vec::new(),
                ignore_config: PubspecIgnoreConfig::default(),
                generation: 0,
                dart: None,
            }),
            DocumentKind::Dart => {
                let project = find_project_for_path(&path);
                let role: DartFileRole = classify_dart_file_role(
                    &path,
                    project.as_ref().map(|p| p.root.as_path()),
                    text,
                );
                Some(Self {
                    kind,
                    text: text.to_string(),
                    project,
                    pubspec: None,
                    locked_versions: std::collections::HashMap::new(),
                    evaluations: Vec::new(),
                    ignore_config: PubspecIgnoreConfig::default(),
                    generation: 0,
                    dart: Some(parse_dart_file(text, role)),
                })
            }
        }
    }
}

pub fn classify_document_path(path: &Path) -> DocumentKind {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    match file_name {
        "pubspec.yaml" => DocumentKind::Pubspec,
        "pubspec_overrides.yaml" => DocumentKind::PubspecOverrides,
        _ if file_name.ends_with(".dart") => DocumentKind::Dart,
        _ => DocumentKind::Ignored,
    }
}

pub fn uri_to_file_path(uri: &Uri) -> Option<PathBuf> {
    let parsed = url::Url::parse(uri.as_str()).ok()?;
    parsed.to_file_path().ok()
}

pub fn file_path_to_uri(path: &Path) -> Option<Uri> {
    let url = url::Url::from_file_path(path).ok()?;
    url.as_str().parse::<Uri>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn apply_json_patch_deep_merges_pubspec_and_preserves_on_empty_object() {
        let mut cfg = ServerConfig::default();
        assert!(cfg.pubspec.enabled);
        assert!(cfg.pubspec.auto_pub_get_on_save);
        assert!(cfg.pubspec.show_up_to_date_hints);
        assert_eq!(cfg.pubspec.major_severity, SeverityLevel::Warning);
        assert_eq!(cfg.pubspec.minor_patch_severity, SeverityLevel::Info);
        assert_eq!(cfg.pubspec.lock_drift_severity, SeverityLevel::Info);
        assert!(!cfg.pubspec.pub_upgrade_tighten);

        assert!(cfg.apply_json_patch(&json!({
            "closingLabels": false,
            "closingLabelsMinLines": 4,
            "pubspec": {
                "autoPubGetOnSave": false,
                "pubUpgradeTighten": true,
                "majorSeverity": "error",
                "lockDriftSeverity": "hint",
                "ignorePackages": ["legacy_pkg"]
            }
        })));
        assert!(!cfg.closing_labels);
        assert_eq!(cfg.closing_labels_min_lines, 4);
        assert!(!cfg.pubspec.auto_pub_get_on_save);
        assert!(cfg.pubspec.pub_upgrade_tighten);
        assert_eq!(cfg.pubspec.major_severity, SeverityLevel::Error);
        assert_eq!(cfg.pubspec.lock_drift_severity, SeverityLevel::Hint);
        assert_eq!(cfg.pubspec.ignore_packages, vec!["legacy_pkg".to_string()]);
        // Unmentioned sibling fields inside `pubspec` must remain untouched
        assert!(cfg.pubspec.enabled);
        assert!(cfg.pubspec.code_lens_pub_get);
        assert_eq!(cfg.pubspec.minor_patch_severity, SeverityLevel::Info);

        // Empty `{}` or `null` from `didChangeConfiguration` MUST NOT reset anything
        assert!(!cfg.apply_json_patch(&json!({})));
        assert!(!cfg.apply_json_patch(&Value::Null));
        assert!(!cfg.pubspec.auto_pub_get_on_save);
        assert_eq!(cfg.pubspec.major_severity, SeverityLevel::Error);
    }

    #[test]
    fn classifies_only_dart_and_pubspec_files() {
        assert_eq!(
            classify_document_path(Path::new("/repo/.github/workflows/ci.yml")),
            DocumentKind::Ignored
        );
        assert_eq!(
            classify_document_path(Path::new("/repo/pubspec.yaml")),
            DocumentKind::Pubspec
        );
        assert_eq!(
            classify_document_path(Path::new("/repo/pubspec_overrides.yaml")),
            DocumentKind::PubspecOverrides
        );
        assert_eq!(
            classify_document_path(Path::new("/repo/test/widget_test.dart")),
            DocumentKind::Dart
        );
    }
}
