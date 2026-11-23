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

/// Result of resolving which Flutter device a DAP or `ctl run` session should target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDevice {
    pub id: String,
    pub auto_selected_notice: Option<String>,
}

/// Extracts a manually supplied device ID from CLI/tool arguments (`-d <id>`, `-d=<id>`,
/// `--device-id <id>`, or `--device-id=<id>`).
pub fn extract_device_flag_from_args(args: &[String]) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "-d" || arg == "--device-id" {
            if let Some(val) = iter.next()
                && !val.trim().is_empty()
            {
                return Some(val.trim().to_string());
            }
        } else if let Some(val) = arg
            .strip_prefix("--device-id=")
            .or_else(|| arg.strip_prefix("-d="))
            && !val.trim().is_empty()
        {
            return Some(val.trim().to_string());
        }
    }
    None
}

/// Resolves the effective Flutter device ID for `project_root`:
/// 1. Explicit device (`deviceId` in `.zed/debug.json` or CLI `--device`) always wins.
/// 2. Persisted `selected_devices.json` selection is respected unless a fresh (<=120s)
///    non-empty `devices.json` cache proves it is offline, in which case an online
///    platform-compatible device is auto-selected with a user-facing notice.
/// 3. If no device was ever selected, auto-selects the highest-priority platform-compatible
///    device from `devices.json` with a user-facing notice.
pub fn resolve_effective_device(
    cache: &DiskCache,
    project_root: &Path,
    explicit_device: Option<&str>,
) -> Option<ResolvedDevice> {
    if let Some(dev) = explicit_device
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(ResolvedDevice {
            id: dev.to_string(),
            auto_selected_notice: None,
        });
    }

    let selected = cache.get_selected_device(project_root);
    let device_cache = cache.load_devices();

    if let Some(selected_id) = selected {
        let cache_proves_offline = device_cache.as_ref().is_some_and(|dc| {
            !dc.devices.is_empty()
                && is_fresh(dc.fetched_at_unix, 120)
                && !dc.devices.iter().any(|d| d.id == selected_id)
        });

        if !cache_proves_offline {
            return Some(ResolvedDevice {
                id: selected_id,
                auto_selected_notice: None,
            });
        }

        if let Some(dc) = device_cache.as_ref()
            && let Some(best) = pick_best_default_device(project_root, &dc.devices)
        {
            return Some(ResolvedDevice {
                id: best.id.clone(),
                auto_selected_notice: Some(format!(
                    "Previously selected device \"{selected_id}\" is offline; auto-selected {} ({}). Run \"Flutter: Select Device\" to switch.",
                    best.name, best.id
                )),
            });
        }

        return Some(ResolvedDevice {
            id: selected_id,
            auto_selected_notice: None,
        });
    }

    if let Some(dc) = device_cache
        && !dc.devices.is_empty()
        && let Some(best) = pick_best_default_device(project_root, &dc.devices)
    {
        return Some(ResolvedDevice {
            id: best.id.clone(),
            auto_selected_notice: Some(format!(
                "No target device selected for this project; auto-selected {} ({}). Run \"Flutter: Select Device\" to switch.",
                best.name, best.id
            )),
        });
    }

    None
}

fn device_platform_dir(device: &FlutterDevice) -> Option<&'static str> {
    let plat = device.target_platform.to_ascii_lowercase();
    let id = device.id.to_ascii_lowercase();

    if plat.contains("darwin") || plat.contains("macos") || id == "macos" {
        Some("macos")
    } else if plat.contains("ios") {
        Some("ios")
    } else if plat.contains("android") {
        Some("android")
    } else if plat.contains("web") || matches!(id.as_str(), "chrome" | "edge" | "web-server") {
        Some("web")
    } else if plat.contains("linux") || id == "linux" {
        Some("linux")
    } else if plat.contains("windows") || id == "windows" {
        Some("windows")
    } else {
        None
    }
}

fn device_priority_tier(device: &FlutterDevice) -> u8 {
    if device.emulator {
        return 0;
    }

    let plat = device.target_platform.to_ascii_lowercase();
    let id = device.id.to_ascii_lowercase();
    let name_lower = device.name.to_ascii_lowercase();

    let is_mobile = plat.contains("ios") || plat.contains("android");
    if is_mobile {
        return if name_lower.contains("wireless") {
            3
        } else {
            1
        };
    }

    let is_desktop = plat.contains("darwin")
        || plat.contains("macos")
        || plat.contains("linux")
        || plat.contains("windows")
        || matches!(id.as_str(), "macos" | "linux" | "windows");
    if is_desktop {
        return 2;
    }

    let is_web = plat.contains("web") || matches!(id.as_str(), "chrome" | "edge" | "web-server");
    if is_web {
        return 4;
    }

    5
}

fn pick_best_default_device<'a>(
    project_root: &Path,
    devices: &'a [FlutterDevice],
) -> Option<&'a FlutterDevice> {
    const PLATFORM_DIRS: [&str; 6] = ["macos", "ios", "android", "web", "linux", "windows"];
    let enabled_platforms: Vec<&str> = PLATFORM_DIRS
        .iter()
        .copied()
        .filter(|dir| project_root.join(dir).is_dir())
        .collect();

    let candidates: Vec<&FlutterDevice> = if enabled_platforms.is_empty() {
        devices.iter().collect()
    } else {
        devices
            .iter()
            .filter(|dev| {
                device_platform_dir(dev).is_some_and(|p| enabled_platforms.contains(&p))
            })
            .collect()
    };

    candidates
        .into_iter()
        .min_by_key(|dev| device_priority_tier(dev))
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
