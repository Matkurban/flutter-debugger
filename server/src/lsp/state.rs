use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
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
    /// Synchronously classifies and parses a document so `documentLink`, `codeLens`,
    /// `documentColor`, and `inlayHint` immediately have parsed data right after `didOpen`.
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

    #[test]
    fn classifies_only_dart_and_pubspec_files() {
        assert_eq!(
            classify_document_path(Path::new("/repo/.github/workflows/ci.yml")),
            DocumentKind::Ignored
        );
        assert_eq!(
            classify_document_path(Path::new("/repo/package.json")),
            DocumentKind::Ignored
        );
        assert_eq!(
            classify_document_path(Path::new("/var/log/system.log")),
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
