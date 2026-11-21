use std::fs;
use std::path::PathBuf;

use serde_json::json;

use crate::project::{ProjectInfo, ProjectKind};

/// Creates `.zed/debug.json` with ready-to-use Flutter/Dart DAP presets if it does not exist yet.
/// Returns `(path, created_new)` so an existing user configuration is never overwritten.
pub fn ensure_zed_debug_config(
    project: &ProjectInfo,
    selected_device_id: Option<&str>,
) -> std::io::Result<(PathBuf, bool)> {
    let zed_dir = project.root.join(".zed");
    let config_path = zed_dir.join("debug.json");
    if config_path.is_file() {
        return Ok((config_path, false));
    }

    fs::create_dir_all(&zed_dir)?;
    let content = build_default_debug_json(project, selected_device_id);
    fs::write(&config_path, content)?;
    Ok((config_path, true))
}

pub fn build_default_debug_json(project: &ProjectInfo, selected_device_id: Option<&str>) -> String {
    let entries = match project.kind {
        ProjectKind::Flutter => {
            let mut debug_cfg = json!({
                "adapter": "Flutter",
                "label": "Flutter: Debug",
                "request": "launch",
                "program": "lib/main.dart",
                "flutterMode": "debug"
            });
            let mut profile_cfg = json!({
                "adapter": "Flutter",
                "label": "Flutter: Profile",
                "request": "launch",
                "program": "lib/main.dart",
                "flutterMode": "profile"
            });
            let mut release_cfg = json!({
                "adapter": "Flutter",
                "label": "Flutter: Release",
                "request": "launch",
                "program": "lib/main.dart",
                "flutterMode": "release"
            });

            if let Some(dev) = selected_device_id
                && !dev.trim().is_empty()
            {
                debug_cfg["deviceId"] = json!(dev);
                profile_cfg["deviceId"] = json!(dev);
                release_cfg["deviceId"] = json!(dev);
            }

            json!([
                debug_cfg,
                profile_cfg,
                release_cfg,
                {
                    "adapter": "Flutter",
                    "label": "Flutter: Attach",
                    "request": "attach"
                },
                {
                    "adapter": "Flutter",
                    "label": "Flutter: Tests",
                    "request": "launch",
                    "program": "test"
                }
            ])
        }
        ProjectKind::Dart => json!([
            {
                "adapter": "Flutter",
                "label": "Dart: Debug",
                "request": "launch",
                "program": "bin/main.dart"
            },
            {
                "adapter": "Flutter",
                "label": "Dart: Tests",
                "request": "launch",
                "program": "test"
            }
        ]),
    };

    serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_flutter_debug_json_without_overwriting_existing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = ProjectInfo {
            root: dir.path().to_path_buf(),
            pubspec_path: dir.path().join("pubspec.yaml"),
            kind: ProjectKind::Flutter,
        };

        let (path, created) =
            ensure_zed_debug_config(&project, Some("macos")).expect("create debug.json");
        assert!(created);
        let raw = fs::read_to_string(&path).expect("read");
        assert!(raw.contains("\"Flutter: Debug\""));
        assert!(raw.contains("\"Flutter: Profile\""));
        assert!(raw.contains("\"deviceId\": \"macos\""));

        fs::write(&path, "[{\"custom\": true}]").expect("custom write");
        let (_, created_again) =
            ensure_zed_debug_config(&project, Some("ios")).expect("check existing");
        assert!(!created_again);
        assert_eq!(fs::read_to_string(&path).expect("read"), "[{\"custom\": true}]");
    }
}
