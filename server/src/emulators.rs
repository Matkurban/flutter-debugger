use std::path::Path;
use std::process::Stdio;

use crate::process_util::build_command;
use crate::project::{ProjectKind, resolve_sdk_executable};
use crate::runner::strip_ansi_codes;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlutterEmulator {
    pub id: String,
    pub name: String,
    pub platform: String,
}

impl FlutterEmulator {
    pub fn display_label(&self) -> String {
        if self.platform.is_empty() {
            format!("{} ({})", self.name, self.id)
        } else {
            format!("{} [{}]", self.name, self.platform)
        }
    }
}

/// Parses the bullet/table output of `flutter emulators`.
pub fn parse_emulators_output(stdout: &str) -> Vec<FlutterEmulator> {
    let mut results = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.contains('•') {
            continue;
        }
        let parts: Vec<&str> = trimmed.split('•').map(str::trim).collect();
        if parts.len() >= 2 {
            let id = parts[0];
            if id.is_empty() || id.eq_ignore_ascii_case("id") {
                continue;
            }
            let name = parts[1];
            let platform = parts.last().copied().unwrap_or_default();
            results.push(FlutterEmulator {
                id: id.to_string(),
                name: name.to_string(),
                platform: platform.to_string(),
            });
        }
    }
    results
}

pub async fn list_emulators(
    cwd: &Path,
    custom_sdk_path: Option<&str>,
) -> Result<Vec<FlutterEmulator>, String> {
    let program = resolve_sdk_executable(cwd, ProjectKind::Flutter, custom_sdk_path);
    let output = build_command(&program, ["emulators"], cwd)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|err| format!("Failed to run `{program} emulators`: {err}"))?;

    let stdout = strip_ansi_codes(&String::from_utf8_lossy(&output.stdout));
    Ok(parse_emulators_output(&stdout))
}

pub async fn launch_emulator(
    cwd: &Path,
    emulator_id: &str,
    custom_sdk_path: Option<&str>,
) -> Result<(), String> {
    if emulator_id.trim().is_empty()
        || !emulator_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(format!("Invalid emulator ID `{emulator_id}`"));
    }

    let program = resolve_sdk_executable(cwd, ProjectKind::Flutter, custom_sdk_path);
    let output = build_command(&program, ["emulators", "--launch", emulator_id], cwd)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|err| format!("Failed to launch emulator `{emulator_id}`: {err}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = strip_ansi_codes(&String::from_utf8_lossy(&output.stderr));
        let stdout = strip_ansi_codes(&String::from_utf8_lossy(&output.stdout));
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        Err(format!("Failed to launch `{emulator_id}`: {detail}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flutter_emulators_list() {
        let sample = r#"
2 available emulators:

Id                   • Name                 • Manufacturer • Platform

apple_ios_simulator  • iOS Simulator        • Apple        • ios
Pixel_8_API_35       • Pixel 8 API 35       • Google       • android
"#;
        let emulators = parse_emulators_output(sample);
        assert_eq!(emulators.len(), 2);
        assert_eq!(emulators[0].id, "apple_ios_simulator");
        assert_eq!(emulators[1].id, "Pixel_8_API_35");
    }
}
