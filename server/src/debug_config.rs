use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{Value, json};

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

    let (debug_path, debug_added) = sync_zed_debug_json(&project, force)
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
pub fn sync_zed_debug_json(project: &ProjectInfo, force: bool) -> io::Result<(PathBuf, usize)> {
    let zed_dir = project.root.join(".zed");
    fs::create_dir_all(&zed_dir)?;
    let path = zed_dir.join("debug.json");
    let presets = build_debug_presets(project);
    let added = merge_json_array_file(&path, &presets, force)?;
    Ok((path, added))
}

/// Losslessly creates or merges `.zed/tasks.json` preserving custom user tasks by `label`.
pub fn sync_zed_tasks_json(project: &ProjectInfo, force: bool) -> io::Result<(PathBuf, usize)> {
    let zed_dir = project.root.join(".zed");
    fs::create_dir_all(&zed_dir)?;
    let path = zed_dir.join("tasks.json");
    let tasks = build_tasks_presets(project);
    let added = merge_json_array_file(&path, &tasks, force)?;
    Ok((path, added))
}

/// Ensures `.zed/debug.json` and `.zed/tasks.json` exist for the LSP command.
pub fn ensure_zed_debug_config(project: &ProjectInfo) -> io::Result<(PathBuf, bool)> {
    let (debug_path, added) = sync_zed_debug_json(project, false)?;
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
            if !matches!(
                name,
                "create" | "register" | "named" | "resValue" | "manifestPlaceholders"
            ) {
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

/// Builds default `.zed/debug.json` presets WITHOUT hardcoding `deviceId` (so presets always
/// dynamically follow `flutter-debugger-server ctl select-device`) and without duplicate labels
/// when both `lib/main_<flavor>.dart` and flavor `<flavor>` exist.
pub fn build_debug_presets(project: &ProjectInfo) -> Vec<Value> {
    match project.kind {
        ProjectKind::Flutter => {
            let mut entries = Vec::new();
            let main_files = detect_main_entrypoints(&project.root);
            let flavors = detect_flavors(&project.root);

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
                if let Some(f) = flavor {
                    obj["flavor"] = json!(f);
                }
                obj
            };

            entries.push(make_app("Flutter: Debug", "debug", &primary_entry, None));
            entries.push(make_app("Flutter: Profile", "profile", &primary_entry, None));
            entries.push(make_app("Flutter: Release", "release", &primary_entry, None));

            let mut paired_entries: HashSet<String> = HashSet::new();

            // Pair `lib/main_<flavor>.dart` with matching flavor into a single smart preset
            for flavor in &flavors {
                let matching_entry = format!("lib/main_{flavor}.dart");
                if main_files.iter().any(|e| e == &matching_entry) {
                    paired_entries.insert(matching_entry.clone());
                    entries.push(make_app(
                        &format!("Flutter: Debug (flavor: {flavor})"),
                        "debug",
                        &matching_entry,
                        Some(flavor),
                    ));
                } else {
                    entries.push(make_app(
                        &format!("Flutter: Debug (flavor: {flavor})"),
                        "debug",
                        &primary_entry,
                        Some(flavor),
                    ));
                }
            }

            // Remaining unpaired `lib/main_*.dart` entrypoints
            for entry in &main_files {
                if entry != &primary_entry && !paired_entries.contains(entry) {
                    let stem = entry.trim_start_matches("lib/").trim_end_matches(".dart");
                    entries.push(make_app(
                        &format!("Flutter: Debug ({stem})"),
                        "debug",
                        entry,
                        None,
                    ));
                }
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

/// Strips JSONC comments (`// ...`, `/* ... */`) and trailing commas (`,}` / `,]`)
/// while strictly preserving string literals.
pub fn strip_jsonc(raw: &str) -> String {
    // Pass 1: Strip comments outside string literals
    let bytes = raw.as_bytes();
    let len = bytes.len();
    let mut no_comments = String::with_capacity(len);
    let mut i = 0usize;
    let mut in_string = false;

    while i < len {
        let b = bytes[i];
        if in_string {
            if b == b'\\' && i + 1 < len {
                no_comments.push('\\');
                let ch = raw[i + 1..].chars().next().unwrap();
                no_comments.push(ch);
                i += 1 + ch.len_utf8();
                continue;
            }
            if b == b'"' {
                in_string = false;
            }
            let ch = raw[i..].chars().next().unwrap();
            no_comments.push(ch);
            i += ch.len_utf8();
            continue;
        }

        if b == b'"' {
            in_string = true;
            no_comments.push('"');
            i += 1;
            continue;
        }

        // Line comment `// ...`
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            i += 2;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        // Block comment `/* ... */`
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] == b'\n' {
                    no_comments.push('\n');
                }
                i += 1;
            }
            if i + 1 < len {
                i += 2;
            }
            continue;
        }

        let ch = raw[i..].chars().next().unwrap();
        no_comments.push(ch);
        i += ch.len_utf8();
    }

    // Pass 2: Strip trailing commas before `}` or `]` outside string literals
    let nc_bytes = no_comments.as_bytes();
    let nc_len = nc_bytes.len();
    let mut out = String::with_capacity(nc_len);
    let mut j = 0usize;
    in_string = false;

    while j < nc_len {
        let b = nc_bytes[j];
        if in_string {
            if b == b'\\' && j + 1 < nc_len {
                out.push('\\');
                let ch = no_comments[j + 1..].chars().next().unwrap();
                out.push(ch);
                j += 1 + ch.len_utf8();
                continue;
            }
            if b == b'"' {
                in_string = false;
            }
            let ch = no_comments[j..].chars().next().unwrap();
            out.push(ch);
            j += ch.len_utf8();
            continue;
        }

        if b == b'"' {
            in_string = true;
            out.push('"');
            j += 1;
            continue;
        }

        if b == b',' {
            // Look ahead past whitespace for `}` or `]`
            let mut k = j + 1;
            while k < nc_len && nc_bytes[k].is_ascii_whitespace() {
                k += 1;
            }
            if k < nc_len && (nc_bytes[k] == b'}' || nc_bytes[k] == b']') {
                j += 1;
                continue;
            }
        }

        let ch = no_comments[j..].chars().next().unwrap();
        out.push(ch);
        j += ch.len_utf8();
    }

    out
}

/// Losslessly merges `desired` objects into a JSON/JSONC array file keyed by `"label"`.
/// Refuses to overwrite if the existing file cannot be parsed as a valid JSON array.
pub fn merge_json_array_file(
    path: &Path,
    desired: &[Value],
    force: bool,
) -> io::Result<usize> {
    let mut existing: Vec<Value> = if path.is_file() {
        let raw = fs::read_to_string(path)?;
        if raw.trim().is_empty() {
            Vec::new()
        } else {
            let cleaned = strip_jsonc(&raw);
            serde_json::from_str::<Vec<Value>>(&cleaned).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Refusing to overwrite `{}` because it is not a valid JSON/JSONC array ({err}). Please fix syntax manually.",
                        path.display()
                    ),
                )
            })?
        }
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
    fn strips_jsonc_comments_and_trailing_commas_preserving_strings() {
        let raw = r#"
        // top comment
        [
          {
            "label": "Custom // keep http://url",
            /* block comment */
            "adapter": "Flutter",
          },
        ]
        "#;
        let cleaned = strip_jsonc(raw);
        let parsed: Vec<Value> = serde_json::from_str(&cleaned).expect("valid json");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["label"], "Custom // keep http://url");
    }

    #[test]
    fn refuses_to_overwrite_invalid_json_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("debug.json");
        let broken = "[ { \"label\": \"Broken\" ";
        fs::write(&path, broken).expect("write broken");

        let res = merge_json_array_file(&path, &[json!({"label": "New"})], false);
        assert!(res.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), broken);
    }

    #[test]
    fn does_not_hardcode_device_id_and_deduplicates_main_dev_with_flavor_dev() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("lib")).expect("mkdir lib");
        fs::create_dir_all(dir.path().join("android/app")).expect("mkdir android");
        fs::write(dir.path().join("lib/main.dart"), "void main() {}").expect("main");
        fs::write(dir.path().join("lib/main_dev.dart"), "void main() {}").expect("main_dev");
        fs::write(dir.path().join("lib/main_storybook.dart"), "void main() {}").expect("sb");
        fs::write(
            dir.path().join("android/app/build.gradle.kts"),
            r#"android { productFlavors { create("dev") {} create("prod") {} } }"#,
        )
        .expect("gradle");

        let project = ProjectInfo {
            root: dir.path().to_path_buf(),
            pubspec_path: dir.path().join("pubspec.yaml"),
            kind: ProjectKind::Flutter,
        };

        let presets = build_debug_presets(&project);
        // No preset should hardcode deviceId
        assert!(presets.iter().all(|p| p.get("deviceId").is_none()));

        // All labels must be unique
        let labels: Vec<&str> = presets
            .iter()
            .filter_map(|p| p.get("label").and_then(Value::as_str))
            .collect();
        let unique: HashSet<&str> = labels.iter().copied().collect();
        assert_eq!(labels.len(), unique.len());

        // `dev` flavor should be smartly paired with `lib/main_dev.dart`
        let dev_preset = presets
            .iter()
            .find(|p| p["label"] == "Flutter: Debug (flavor: dev)")
            .expect("dev preset");
        assert_eq!(dev_preset["program"], "lib/main_dev.dart");
        assert_eq!(dev_preset["flavor"], "dev");

        // Unpaired `main_storybook.dart` should have its own label
        assert!(presets
            .iter()
            .any(|p| p["label"] == "Flutter: Debug (main_storybook)"));
    }

    #[test]
    fn losslessly_merges_jsonc_debug_and_tasks_json() {
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
            r#"[
              // user custom preset
              {
                "label": "My Custom Entry",
                "adapter": "Flutter",
                "request": "launch",
              },
            ]"#,
        )
        .expect("write custom jsonc");

        let (debug_path, added) = sync_zed_debug_json(&project, false).expect("sync debug");
        assert!(added >= 5);
        let parsed: Vec<Value> =
            serde_json::from_str(&fs::read_to_string(&debug_path).unwrap()).unwrap();
        assert!(parsed.iter().any(|v| v["label"] == "My Custom Entry"));
        assert!(parsed
            .iter()
            .any(|v| v["label"] == "Flutter: Current Test (Breakpoints)" && v["isTest"] == true));
    }
}
