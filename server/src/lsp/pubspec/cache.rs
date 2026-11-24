use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use serde::{Serialize, de::DeserializeOwned};
use tracing::debug;

use crate::cache::now_unix;
use crate::lsp::pubspec::registry::{CachedChangelog, PackageInfo};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Two-tier (in-memory DashMap + atomic persistent disk cache) store for pub package metadata
/// and parsed changelogs under `<DiskCache::root_dir()>/pub_packages` and `pub_changelogs`.
#[derive(Debug)]
pub struct PubPackageCache {
    packages: DashMap<String, PackageInfo>,
    changelogs: DashMap<String, CachedChangelog>,
    disk_dir: PathBuf,
}

impl PubPackageCache {
    #[must_use]
    pub fn new(cache_root: &Path) -> Self {
        let _ = std::fs::create_dir_all(cache_root.join("pub_packages"));
        let _ = std::fs::create_dir_all(cache_root.join("pub_changelogs"));
        Self {
            packages: DashMap::new(),
            changelogs: DashMap::new(),
            disk_dir: cache_root.to_path_buf(),
        }
    }

    #[must_use]
    pub fn cache_key(hosted_url: &str, package: &str) -> String {
        format!("{}:{}", hosted_url.trim_end_matches('/'), package)
    }

    pub async fn get_package(
        &self,
        hosted_url: &str,
        package: &str,
        ttl_secs: u64,
    ) -> Option<PackageInfo> {
        let key = Self::cache_key(hosted_url, package);
        let now = now_unix();

        if let Some(entry) = self.packages.get(&key)
            && now.saturating_sub(entry.fetched_at_unix) <= ttl_secs
        {
            return Some(entry.clone());
        }

        let path = self.package_file_path(&key);
        if let Some(info) = read_json_file::<PackageInfo>(&path).await
            && now.saturating_sub(info.fetched_at_unix) <= ttl_secs
        {
            self.packages.insert(key, info.clone());
            return Some(info);
        }

        None
    }

    pub async fn put_package(&self, info: PackageInfo) {
        let key = Self::cache_key(&info.hosted_url, &info.name);
        let path = self.package_file_path(&key);
        write_json_file_atomic(&path, &info).await;
        self.packages.insert(key, info);
    }

    pub async fn get_changelog(
        &self,
        hosted_url: &str,
        package: &str,
        ttl_secs: u64,
    ) -> Option<CachedChangelog> {
        let key = Self::cache_key(hosted_url, package);
        let now = now_unix();

        if let Some(entry) = self.changelogs.get(&key)
            && now.saturating_sub(entry.fetched_at_unix) <= ttl_secs
        {
            return Some(entry.clone());
        }

        let path = self.changelog_file_path(&key);
        if let Some(cl) = read_json_file::<CachedChangelog>(&path).await
            && now.saturating_sub(cl.fetched_at_unix) <= ttl_secs
        {
            self.changelogs.insert(key, cl.clone());
            return Some(cl);
        }

        None
    }

    pub async fn put_changelog(&self, hosted_url: &str, package: &str, cl: CachedChangelog) {
        let key = Self::cache_key(hosted_url, package);
        let path = self.changelog_file_path(&key);
        write_json_file_atomic(&path, &cl).await;
        self.changelogs.insert(key, cl);
    }

    pub async fn invalidate(&self, items: &[(String, String)]) {
        for (hosted_url, package) in items {
            let key = Self::cache_key(hosted_url, package);
            self.packages.remove(&key);
            self.changelogs.remove(&key);

            let _ = tokio::fs::remove_file(self.package_file_path(&key)).await;
            let _ = tokio::fs::remove_file(self.changelog_file_path(&key)).await;
        }
    }

    fn package_file_path(&self, key: &str) -> PathBuf {
        self.disk_dir
            .join("pub_packages")
            .join(format!("{}.json", sanitize_filename(key)))
    }

    fn changelog_file_path(&self, key: &str) -> PathBuf {
        self.disk_dir
            .join("pub_changelogs")
            .join(format!("{}.json", sanitize_filename(key)))
    }
}

fn sanitize_filename(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

async fn read_json_file<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = tokio::fs::read(path).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn write_json_file_atomic<T: Serialize>(path: &Path, value: &T) {
    let Ok(bytes) = serde_json::to_vec(value) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.{}.{seq}.tmp", std::process::id()));
    if let Err(err) = tokio::fs::write(&tmp, &bytes).await {
        debug!(path = %tmp.display(), %err, "failed to write pub cache temp file");
        return;
    }
    if let Err(err) = tokio::fs::rename(&tmp, path).await {
        debug!(path = %path.display(), %err, "failed to rename pub cache temp file");
        let _ = tokio::fs::remove_file(&tmp).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::pubspec::registry::VersionSummary;

    #[tokio::test]
    async fn persists_package_info_atomically_and_supports_invalidation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = PubPackageCache::new(dir.path());
        let info = PackageInfo {
            name: "dio".to_string(),
            hosted_url: "https://pub.dev".to_string(),
            fetched_at_unix: now_unix(),
            is_discontinued: false,
            replaced_by: None,
            description: Some("HTTP client".to_string()),
            repository: None,
            homepage: None,
            latest_stable: Some("5.8.0".to_string()),
            latest_prerelease: None,
            versions: vec![VersionSummary {
                version: "5.8.0".to_string(),
                retracted: false,
                published: None,
                sdk_constraint: Some("^3.0.0".to_string()),
            }],
            score: None,
        };

        cache.put_package(info.clone()).await;

        // Fresh instance reads back from disk cache
        let cache2 = PubPackageCache::new(dir.path());
        let loaded = cache2
            .get_package("https://pub.dev", "dio", 3600)
            .await
            .expect("disk cache hit");
        assert_eq!(loaded.latest_stable.as_deref(), Some("5.8.0"));

        // Invalidation removes both memory and disk entries
        cache2
            .invalidate(&[("https://pub.dev".to_string(), "dio".to_string())])
            .await;
        assert!(
            cache2
                .get_package("https://pub.dev", "dio", 3600)
                .await
                .is_none()
        );
    }
}
