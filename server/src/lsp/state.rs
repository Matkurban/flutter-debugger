use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower_lsp_server::ls_types::Uri;

use crate::cache::{DEFAULT_DEVICE_CACHE_TTL_SECS, DEFAULT_PUB_CACHE_TTL_SECS};
use crate::lsp::dart_file::{DartFileRole, ParsedDartFile, classify_dart_file_role, parse_dart_file};
use crate::lsp::pubspec::{ParsedPubspec, parse_pubspec_dependencies};
use crate::project::{ProjectInfo, find_project_for_path};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    #[serde(default = "default_true")]
    pub auto_hot_reload_on_save: bool,
    #[serde(default = "default_debounce_ms")]
    pub hot_reload_debounce_ms: u64,
    #[serde(default = "default_true")]
    pub closing_labels: bool,
    #[serde(default = "default_closing_min_lines")]
    pub closing_labels_min_lines: u32,
    #[serde(default = "default_true")]
    pub color_previews: bool,
    #[serde(default)]
    pub enable_pub_version_hints: bool,
    #[serde(default)]
    pub expose_global_code_actions: bool,
    #[serde(default)]
    pub flutter_sdk_path: Option<String>,
    #[serde(default = "default_pub_timeout")]
    pub pub_dev_timeout_secs: u64,
    #[serde(default = "default_device_ttl")]
    pub device_cache_ttl_secs: u64,
    #[serde(default = "default_pub_ttl")]
    pub pub_cache_ttl_secs: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigPatch {
    pub auto_hot_reload_on_save: Option<bool>,
    pub hot_reload_debounce_ms: Option<u64>,
    pub closing_labels: Option<bool>,
    pub closing_labels_min_lines: Option<u32>,
    pub color_previews: Option<bool>,
    pub enable_pub_version_hints: Option<bool>,
    pub expose_global_code_actions: Option<bool>,
    pub flutter_sdk_path: Option<String>,
    pub pub_dev_timeout_secs: Option<u64>,
    pub device_cache_ttl_secs: Option<u64>,
    pub pub_cache_ttl_secs: Option<u64>,
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
        let Ok(patch) = serde_json::from_value::<ServerConfigPatch>(value.clone()) else {
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
        if let Some(v) = patch.enable_pub_version_hints {
            self.enable_pub_version_hints = v;
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
        if let Some(v) = patch.pub_dev_timeout_secs {
            self.pub_dev_timeout_secs = v;
            changed = true;
        }
        if let Some(v) = patch.device_cache_ttl_secs {
            self.device_cache_ttl_secs = v;
            changed = true;
        }
        if let Some(v) = patch.pub_cache_ttl_secs {
            self.pub_cache_ttl_secs = v;
            changed = true;
        }
        changed
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            auto_hot_reload_on_save: true,
            hot_reload_debounce_ms: default_debounce_ms(),
            closing_labels: true,
            closing_labels_min_lines: default_closing_min_lines(),
            color_previews: true,
            enable_pub_version_hints: false,
            expose_global_code_actions: false,
            flutter_sdk_path: None,
            pub_dev_timeout_secs: default_pub_timeout(),
            device_cache_ttl_secs: default_device_ttl(),
            pub_cache_ttl_secs: default_pub_ttl(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_debounce_ms() -> u64 {
    250
}

fn default_closing_min_lines() -> u32 {
    2
}

fn default_pub_timeout() -> u64 {
    10
}

fn default_device_ttl() -> u64 {
    DEFAULT_DEVICE_CACHE_TTL_SECS
}

fn default_pub_ttl() -> u64 {
    DEFAULT_PUB_CACHE_TTL_SECS
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
                dart: None,
            }),
            DocumentKind::Pubspec | DocumentKind::PubspecOverrides => Some(Self {
                kind,
                text: text.to_string(),
                project: find_project_for_path(&path),
                pubspec: Some(parse_pubspec_dependencies(text)),
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
    fn apply_json_patch_preserves_existing_settings_on_empty_object() {
        let mut cfg = ServerConfig::default();
        assert!(cfg.apply_json_patch(&json!({
            "closingLabels": false,
            "closingLabelsMinLines": 4,
            "enablePubVersionHints": true
        })));
        assert!(!cfg.closing_labels);
        assert_eq!(cfg.closing_labels_min_lines, 4);
        assert!(cfg.enable_pub_version_hints);

        // Empty `{}` or `null` from `didChangeConfiguration` MUST NOT reset anything
        assert!(!cfg.apply_json_patch(&json!({})));
        assert!(!cfg.apply_json_patch(&Value::Null));
        assert!(!cfg.closing_labels);
        assert_eq!(cfg.closing_labels_min_lines, 4);
        assert!(cfg.enable_pub_version_hints);
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
            classify_document_path(Path::new("/repo/test/widget_test.dart")),
            DocumentKind::Dart
        );
    }
}
