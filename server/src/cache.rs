use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const DEFAULT_DEVICE_CACHE_TTL_SECS: u64 = 60 * 60; // 1 hour
pub const DEFAULT_PUB_CACHE_TTL_SECS: u64 = 12 * 60 * 60; // 12 hours

/// Subset of fields returned by `flutter devices --machine`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlutterDevice {
    pub id: String,
    pub name: String,
    #[serde(default, rename = "targetPlatform")]
    pub target_platform: String,
    #[serde(default)]
    pub emulator: bool,
    #[serde(default)]
    pub sdk: String,
}

impl FlutterDevice {
    pub fn display_label(&self) -> String {
        if self.target_platform.is_empty() {
            format!("{} ({})", self.name, self.id)
        } else {
            format!("{} [{}]", self.name, self.target_platform)
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceCache {
    pub fetched_at_unix: u64,
    pub devices: Vec<FlutterDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedPackageVersion {
    pub latest_version: String,
    pub fetched_at_unix: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PubVersionCache {
    pub packages: HashMap<String, CachedPackageVersion>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SelectedDevicesStore {
    pub by_project: HashMap<String, String>,
}

/// Disk-backed cache stored under the OS user cache directory.
#[derive(Debug, Clone)]
pub struct DiskCache {
    root_dir: PathBuf,
}

impl DiskCache {
    pub fn default_location() -> Self {
        let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
        Self {
            root_dir: base.join("flutter-debugger"),
        }
    }

    #[cfg(test)]
    pub fn with_root(root_dir: PathBuf) -> Self {
        Self { root_dir }
    }

    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    pub fn load_devices(&self) -> Option<DeviceCache> {
        self.read_json(&self.root_dir.join("devices.json"))
    }

    pub fn save_devices(&self, devices: &[FlutterDevice]) {
        let cache = DeviceCache {
            fetched_at_unix: now_unix(),
            devices: devices.to_vec(),
        };
        let _ = self.write_json(&self.root_dir.join("devices.json"), &cache);
    }

    pub fn load_pub_versions(&self) -> PubVersionCache {
        self.read_json(&self.root_dir.join("pub_versions.json"))
            .unwrap_or_default()
    }

    pub fn save_pub_versions(&self, cache: &PubVersionCache) {
        let _ = self.write_json(&self.root_dir.join("pub_versions.json"), cache);
    }

    pub fn get_selected_device(&self, project_root: &Path) -> Option<String> {
        let store: SelectedDevicesStore = self
            .read_json(&self.root_dir.join("selected_devices.json"))
            .unwrap_or_default();
        store
            .by_project
            .get(&normalize_project_key(project_root))
            .cloned()
    }

    pub fn set_selected_device(&self, project_root: &Path, device_id: &str) {
        let path = self.root_dir.join("selected_devices.json");
        let mut store: SelectedDevicesStore = self.read_json(&path).unwrap_or_default();
        store
            .by_project
            .insert(normalize_project_key(project_root), device_id.to_string());
        let _ = self.write_json(&path, &store);
    }

    fn read_json<T: for<'de> Deserialize<'de>>(&self, path: &Path) -> Option<T> {
        let bytes = fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn write_json<T: Serialize>(&self, path: &Path, value: &T) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_vec_pretty(value)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, data)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn is_fresh(fetched_at_unix: u64, ttl_secs: u64) -> bool {
    if fetched_at_unix == 0 {
        return false;
    }
    now_unix().saturating_sub(fetched_at_unix) < ttl_secs
}

pub fn normalize_project_key(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_devices_and_selected_device() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::with_root(dir.path().to_path_buf());

        let devices = vec![FlutterDevice {
            id: "macos".to_string(),
            name: "macOS".to_string(),
            target_platform: "darwin".to_string(),
            emulator: false,
            sdk: "macOS 15".to_string(),
        }];
        cache.save_devices(&devices);

        let loaded = cache.load_devices().expect("loaded devices");
        assert_eq!(loaded.devices, devices);
        assert!(is_fresh(loaded.fetched_at_unix, DEFAULT_DEVICE_CACHE_TTL_SECS));

        cache.set_selected_device(dir.path(), "macos");
        assert_eq!(
            cache.get_selected_device(dir.path()).as_deref(),
            Some("macos")
        );
    }

    #[test]
    fn freshness_check_handles_zero_and_expired() {
        assert!(!is_fresh(0, 60));
        assert!(is_fresh(now_unix(), 60));
        assert!(!is_fresh(now_unix().saturating_sub(120), 60));
    }
}
