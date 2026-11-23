use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{Value, json};

use crate::cache::DiskCache;
use crate::project::{ProjectInfo, ProjectKind, find_project_for_path};

/// CLI entrypoint for `flutter-debugger-server init-zed [--project <dir>] [--force]`.
pub fn run_init_zed(args: &[String]) -> Result<(), String> {
    let mut project_dir: Option<PathBuf> = None;
    let mut force = false;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--force" | "-f" => force = true,
            "--project" | "-p" => {
                if let Some(p) = iter.next() {
                    project_dir = Some(PathBuf::from(p));
                }
            }
            other if other.starts_with("--project=") => {
                project_dir = Some(PathBuf::from(other.trim_start_matches("--project=")));
            }
            _ => {}
        }
    }

    let base = project_dir
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let project = find_project_for_path(&base)
        .ok_or_else(|| format!("No pubspec.yaml found in or above `{}`", base.display()))?;

    let cache = DiskCache::default_location();
    let selected_device = cache.get_selected_device(&project.root);

    let (debug_path, debug_added) =
        sync_zed_debug_json(&project, selected_device.as_deref(), force)
            .map_err(|e| format!("Failed to update .zed/debug.json: {e}"))?;
    let (tasks_path, tasks_added) = sync_zed_tasks_json(&project, force)
        .map_err(|e| format!("Failed to update .zed/tasks.json: {e}"))?;

    println!(
        "Updated {} ({debug_added} preset(s) added/updated)",
        debug_path.display()
    );
    println!(
        "Updated {} ({tasks_added} task(s) added/updated)",
        tasks_path.display()
    );
    Ok(())
}

/// Losslessly creates or merges `.zed/debug.json` preserving custom user entries by `label`.
pub fn sync_zed_debug_json(
    project: &ProjectInfo,
    selected_device_id: Option<&str>,
    force: bool,
) -> std::io::Result<(PathBuf, usize)> {
    let zed_dir = project.root.join(".zed");
    fs::create_dir_all(&zed_dir)?;
    let path = zed_dir.join("debug.json");
    let presets = build_debug_presets(project, selected_device_id);
    let added = merge_json_array_file(&path, &presets, force)?;
    Ok((path, added))
}

/// Losslessly creates or merges `.zed/tasks.json` preserving custom user tasks by `label`.
pub fn sync_zed_tasks_json(project: &ProjectInfo, force: bool) -> std::io::Result<(PathBuf, usize)> {
    let zed_dir = project.root.join(".zed");
    fs::create_dir_all(&zed_dir)?;
    let path = zed_dir.join("tasks.json");
    let tasks = build_tasks_presets(project);
    let added = merge_json_array_file(&path, &tasks, force)?;
    Ok((path, added))
}

/// Kept for LSP command compatibility: ensures `.zed/debug.json` and `.zed/tasks.json` exist.
pub fn ensure_zed_debug_config(
    project: &ProjectInfo,
    selected_device_id: Option<&str>,
) -> std::io::Result<(PathBuf, bool)> {
    let (debug_path, added) = sync_zed_debug_json(project, selected_device_id, false)?;
    let _ = sync_zed_tasks_json(project, false);
    Ok((debug_path, added > 0))
}

/// Scans `android/app/build.gradle(.kts)` and `ios/Runner.xcodeproj/xcshareddata/xcschemes`
/// to discover configured Flutter build flavors.
pub fn detect_flavors(project_root: &Path) -> Vec<String> {
    let mut flavors = BTreeSet::new();

    for gradle_rel in ["android/app/build.gradle", "android/app/build.gradle.kts"] {
        let gradle_path = project_root.join(gradle_rel);
        if let Ok(text) = fs::read_to_string(&gradle_path) {
            extract_gradle_flavors(&text, &mut flavors);
        }
    }

    let schemes_dir = project_root.join("ios/Runner.xcodeproj/xcshareddata/xcschemes");
    if let Ok(entries) = fs::read_dir(&schemes_dir) {
        for entry in entries.flatten() {
            if let Some(stem) = entry.path().file_stem().and_then(|s| s.to_str())
                && !stem.eq_ignore_ascii_case("Runner")
                && !stem.is_empty()
            {
                flavors.insert(stem.to_string());
            }
        }
    }

    flavors.into_iter().collect()
}

fn extract_gradle_flavors(text: &str, out: &mut BTreeSet<String>) {
    let Some(start_idx) = text.find("productFlavors") else {
        return;
    };
    let rest = &text[start_idx..];
    let Some(brace_start) = rest.find('{') else {
        return;
    };

    // Extract balanced block of `productFlavors { ... }`
    let mut depth = 0i32;
    let mut block = String::new();
    for ch in rest[brace_start..].chars() {
        match ch {
            '{' => {
                depth += 1;
                if depth > 1 {
                    block.push(ch);
                }
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                block.push(ch);
            }
            _ => {
                if depth >= 1 {
                    block.push(ch);
                }
            }
        }
    }

    // Matches Kotlin DSL `create("staging")` or Groovy `staging {`
    let kts_re = Regex::new(r#"create\s*\(\s*["']([A-Za-z0-9_-]+)["']\s*\)"#).expect("valid regex");
    let groovy_re = Regex::new(r"(?m)^\s*([A-Za-z][A-Za-z0-9_-]*)\s*\{").expect("valid regex");

    for cap in kts_re.captures_iter(&block) {
        if let Some(m) = cap.get(1) {
            out.insert(m.as_str().to_string());
        }
    }
    for cap in groovy_re.captures_iter(&block) {
        if let Some(m) = cap.get(1) {
            let name = m.as_str();
            if !matches!(name, "create" | "register" | "named" | "resValue" | "manifestPlaceholders")
            {
                out.insert(name.to_string());
            }
        }
    }
}

/// Scans `lib/main*.dart` for alternative entry points (e.g. `lib/main_dev.dart`).
pub fn detect_main_entrypoints(project_root: &Path) -> Vec<String> {
    let lib_dir = project_root.join("lib");
    let Ok(entries) = fs::read_dir(&lib_dir) else {
        return vec!["lib/main.dart".to_string()];
    };
    let mut found = BTreeSet::new();
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str()
            && name.starts_with("main")
            && name.ends_with(".dart")
        {
            found.insert(format!("lib/{name}"));
        }
    }
    if found.is_empty() {
        vec!["lib/main.dart".to_string()]
    } else {
        found.into_iter().collect()
    }
}

pub fn build_debug_presets(
    project: &ProjectInfo,
    selected_device_id: Option<&str>,
) -> Vec<Value> {
    match project.kind {
        ProjectKind::Flutter => {
            let mut entries = Vec::new();
            let main_files = detect_main_entrypoints(&project.root);
            let primary_entry = if main_files.iter().any(|f| f == "lib/main.dart") {
                "lib/main.dart".to_string()
            } else {
                main_files
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "lib/main.dart".to_string())
            };

            let make_app = |label: &str, mode: &str, program: &str, flavor: Option<&str>| {
                let mut obj = json!({
                    "adapter": "Flutter",
                    "label": label,
                    "request": "launch",
                    "program": program,
                    "flutterMode": mode
                });
                if let Some(dev) = selected_device_id
                    && !dev.trim().is_empty()
                {
                    obj["deviceId"] = json!(dev);
                }
                if let Some(f) = flavor {
                    obj["flavor"] = json!(f);
                }
                obj
            };

            entries.push(make_app("Flutter: Debug", "debug", &primary_entry, None));
            entries.push(make_app("Flutter: Profile", "profile", &primary_entry, None));
            entries.push(make_app("Flutter: Release", "release", &primary_entry, None));

            for entry in &main_files {
                if entry != &primary_entry {
                    let suffix = entry
                        .trim_start_matches("lib/main_")
                        .trim_end_matches(".dart");
                    entries.push(make_app(
                        &format!("Flutter: Debug ({suffix})"),
                        "debug",
                        entry,
                        None,
                    ));
                }
            }

            for flavor in detect_flavors(&project.root) {
                entries.push(make_app(
                    &format!("Flutter: Debug ({flavor})"),
                    "debug",
                    &primary_entry,
                    Some(&flavor),
                ));
            }

            entries.push(json!({
                "adapter": "Flutter",
                "label": "Flutter: Attach",
                "request": "attach"
            }));
            entries.push(json!({
                "adapter": "Flutter",
                "label": "Flutter: Current Test (Breakpoints)",
                "request": "launch",
                "program": "$ZED_RELATIVE_FILE",
                "isTest": true
            }));
            entries.push(json!({
                "adapter": "Flutter",
                "label": "Flutter: All Tests (Breakpoints)",
                "request": "launch",
                "program": "test",
                "isTest": true
            }));

            entries
        }
        ProjectKind::Dart => vec![
            json!({
                "adapter": "Flutter",
                "label": "Dart: Debug",
                "request": "launch",
                "program": "bin/main.dart"
            }),
            json!({
                "adapter": "Flutter",
                "label": "Dart: Current Test (Breakpoints)",
                "request": "launch",
                "program": "$ZED_RELATIVE_FILE",
                "isTest": true
            }),
        ],
    }
}

pub fn build_tasks_presets(project: &ProjectInfo) -> Vec<Value> {
    let ctl = "flutter-debugger-server";

    if project.kind == ProjectKind::Dart {
        return vec![
            task_item("Dart: Pub Get", ctl, &["ctl", "pub-get"], "always", false),
            task_item(
                "Dart: Build Runner Build",
                ctl,
                &["ctl", "build-runner", "build"],
                "always",
                false,
            ),
            task_item(
                "Dart: Build Runner Watch",
                ctl,
                &["ctl", "build-runner", "watch"],
                "always",
                false,
            ),
        ];
    }

    vec![
        task_item("Flutter: Run (Debug)", ctl, &["ctl", "run", "--mode", "debug"], "always", false),
        task_item("Flutter: Run (Profile)", ctl, &["ctl", "run", "--mode", "profile"], "always", false),
        task_item("Flutter: Run (Release)", ctl, &["ctl", "run", "--mode", "release"], "always", false),
        task_item("Flutter: Hot Reload", ctl, &["ctl", "hot-reload"], "no_focus", true),
        task_item("Flutter: Hot Restart", ctl, &["ctl", "hot-restart"], "no_focus", true),
        task_item("Flutter: Stop App", ctl, &["ctl", "stop"], "no_focus", true),
        task_item("Flutter: Select Device", ctl, &["ctl", "select-device"], "always", false),
        task_item("Flutter: Launch Emulator", ctl, &["ctl", "launch-emulator"], "always", false),
        task_item("Flutter: Open DevTools (Inspector)", ctl, &["ctl", "devtools", "--page", "inspector"], "no_focus", true),
        task_item("Flutter: Open DevTools (Performance)", ctl, &["ctl", "devtools", "--page", "performance"], "no_focus", true),
        task_item("Flutter: Open DevTools (Network)", ctl, &["ctl", "devtools", "--page", "network"], "no_focus", true),
        task_item("Flutter: Toggle Widget Inspector", ctl, &["ctl", "overlay", "inspector"], "no_focus", true),
        task_item("Flutter: Toggle Performance Overlay", ctl, &["ctl", "overlay", "performance"], "no_focus", true),
        task_item("Flutter: Toggle Debug Paint", ctl, &["ctl", "overlay", "debug-paint"], "no_focus", true),
        task_item("Flutter: Toggle Brightness", ctl, &["ctl", "overlay", "brightness"], "no_focus", true),
        task_item("Flutter: Screenshot", ctl, &["ctl", "screenshot"], "no_focus", true),
        task_item("Flutter: Build Runner Watch", ctl, &["ctl", "build-runner", "watch"], "always", false),
        task_item("Flutter: Build Runner Build", ctl, &["ctl", "build-runner", "build"], "always", false),
        task_item("Flutter: Pub Get", ctl, &["ctl", "pub-get"], "always", false),
        task_item("Flutter: Clean", ctl, &["ctl", "clean"], "always", false),
        task_item("Flutter: Doctor", ctl, &["ctl", "doctor"], "always", false),
    ]
}

fn task_item(
    label: &str,
    command: &str,
    args: &[&str],
    reveal: &str,
    hide_on_success: bool,
) -> Value {
    json!({
        "label": label,
        "command": command,
        "args": args,
        "use_new_terminal": false,
        "allow_concurrent_runs": false,
        "reveal": reveal,
        "hide": if hide_on_success { "on_success" } else { "never" }
    })
}

/// Losslessly merges `desired` objects into a JSON array file keyed by `"label"`.
/// Existing entries with custom user labels are always preserved; matching labels are only
/// overwritten when `force == true`.
pub fn merge_json_array_file(
    path: &Path,
    desired: &[Value],
    force: bool,
) -> std::io::Result<usize> {
    let mut existing: Vec<Value> = if path.is_file() {
        let raw = fs::read_to_string(path)?;
        serde_json::from_str::<Vec<Value>>(&raw).unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut changed = 0usize;
    for item in desired {
        let Some(label) = item.get("label").and_then(Value::as_str) else {
            continue;
        };
        if let Some(pos) = existing
            .iter()
            .position(|e| e.get("label").and_then(Value::as_str) == Some(label))
        {
            if force {
                existing[pos] = item.clone();
                changed += 1;
            }
        } else {
            existing.push(item.clone());
            changed += 1;
        }
    }

    let content = serde_json::to_string_pretty(&existing).unwrap_or_else(|_| "[]".to_string());
    fs::write(path, format!("{content}\n"))?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_gradle_kts_and_groovy_flavors() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("android/app")).expect("mkdir");
        fs::write(
            dir.path().join("android/app/build.gradle.kts"),
            r#"
android {
    productFlavors {
        create("dev") {
            dimension = "env"
        }
        create("prod") {
            dimension = "env"
        }
    }
}
"#,
        )
        .expect("write gradle");

        let flavors = detect_flavors(dir.path());
        assert_eq!(flavors, vec!["dev", "prod"]);
    }

    #[test]
    fn losslessly_merges_debug_and_tasks_json_preserving_custom_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = ProjectInfo {
            root: dir.path().to_path_buf(),
            pubspec_path: dir.path().join("pubspec.yaml"),
            kind: ProjectKind::Flutter,
        };

        let zed_dir = dir.path().join(".zed");
        fs::create_dir_all(&zed_dir).expect("mkdir .zed");
        fs::write(
            zed_dir.join("debug.json"),
            r#"[{"label": "My Custom Entry", "adapter": "Flutter", "request": "launch"}]"#,
        )
        .expect("write custom");

        let (debug_path, added) =
            sync_zed_debug_json(&project, Some("macos"), false).expect("sync debug");
        assert!(added >= 5);
        let parsed: Vec<Value> =
            serde_json::from_str(&fs::read_to_string(&debug_path).unwrap()).unwrap();
        assert!(parsed.iter().any(|v| v["label"] == "My Custom Entry"));
        assert!(parsed
            .iter()
            .any(|v| v["label"] == "Flutter: Current Test (Breakpoints)" && v["isTest"] == true));

        let (tasks_path, tasks_added) = sync_zed_tasks_json(&project, false).expect("sync tasks");
        assert!(tasks_added >= 15);
        let tasks: Vec<Value> =
            serde_json::from_str(&fs::read_to_string(&tasks_path).unwrap()).unwrap();
        assert!(tasks.iter().any(|t| t["label"] == "Flutter: Hot Reload"));
        assert!(tasks.iter().any(|t| t["label"] == "Flutter: Select Device"));
    }
}
