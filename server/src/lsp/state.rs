use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tower_lsp_server::ls_types::Uri;

use crate::cache::{DEFAULT_DEVICE_CACHE_TTL_SECS, DEFAULT_PUB_CACHE_TTL_SECS};
use crate::lsp::dart_file::{DartFileRole, ParsedDartFile, classify_dart_file_role, parse_dart_file};
use crate::lsp::pubspec::{ParsedPubspec, parse_pubspec_dependencies};
use crate::project::{ProjectInfo, find_project_for_path};
use crate::wizard::{ProjectWizardConfig, WIZARD_FILENAME};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    #[serde(default = "default_true")]
    pub auto_hot_reload_on_save: bool,
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
    ProjectWizard,
    RunLog,
    Ignored,
}

#[derive(Debug, Clone)]
pub struct OpenDocument {
    pub kind: DocumentKind,
    pub project: Option<ProjectInfo>,
    pub pubspec: Option<ParsedPubspec>,
    pub dart: Option<ParsedDartFile>,
    pub wizard: Option<ProjectWizardConfig>,
}

impl OpenDocument {
    /// Synchronously classifies and parses a document so `documentLink`, `codeLens`,
    /// and `inlayHint` immediately have parsed data right after `didOpen`.
    pub fn parse_sync(uri: &Uri, text: &str) -> Option<Self> {
        let path = uri_to_file_path(uri)?;
        let kind = classify_document_path(&path);

        match kind {
            DocumentKind::Ignored | DocumentKind::RunLog => Some(Self {
                kind,
                project: None,
                pubspec: None,
                dart: None,
                wizard: None,
            }),
            DocumentKind::ProjectWizard => {
                let wizard = serde_json::from_str::<ProjectWizardConfig>(text).ok();
                Some(Self {
                    kind,
                    project: None,
                    pubspec: None,
                    dart: None,
                    wizard,
                })
            }
            DocumentKind::Pubspec | DocumentKind::PubspecOverrides => Some(Self {
                kind,
                project: find_project_for_path(&path),
                pubspec: Some(parse_pubspec_dependencies(text)),
                dart: None,
                wizard: None,
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
                    project,
                    pubspec: None,
                    dart: Some(parse_dart_file(text, role)),
                    wizard: None,
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
        WIZARD_FILENAME => DocumentKind::ProjectWizard,
        _ if file_name.starts_with("flutter-run-") && file_name.ends_with(".log") => {
            DocumentKind::RunLog
        }
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
    fn classifies_only_target_files_and_ignores_others() {
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
            classify_document_path(Path::new("/tmp/flutter-debugger-logs/flutter-run-debug-1.log")),
            DocumentKind::RunLog
        );
        assert_eq!(
            classify_document_path(Path::new("/tmp/flutter-project-wizard.json")),
            DocumentKind::ProjectWizard
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
