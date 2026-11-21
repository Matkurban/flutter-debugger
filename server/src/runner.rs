use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use dashmap::DashSet;
use regex::Regex;
use tokio::process::Command;

use crate::cache::now_unix;
use crate::project::{ProjectInfo, resolve_sdk_executable};

/// Strict whitelist of external CLI commands allowed to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowedCommand {
    PubGet,
    PubUpgrade,
    PubOutdated,
    Clean,
    Doctor,
    BuildRunnerBuild,
    Screenshot {
        device_id: Option<String>,
        output_path: String,
    },
    TestFile {
        relative_path: String,
        test_name: Option<String>,
    },
}

impl AllowedCommand {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::PubGet => "pub get",
            Self::PubUpgrade => "pub upgrade",
            Self::PubOutdated => "pub outdated",
            Self::Clean => "flutter clean",
            Self::Doctor => "flutter doctor",
            Self::BuildRunnerBuild => "build_runner build",
            Self::Screenshot { .. } => "screenshot",
            Self::TestFile { .. } => "test",
        }
    }

    pub fn always_open_log(&self) -> bool {
        matches!(self, Self::PubOutdated | Self::Doctor | Self::TestFile { .. })
    }

    pub fn build_args(&self, project: &ProjectInfo) -> Vec<String> {
        match self {
            Self::PubGet => vec!["pub".into(), "get".into()],
            Self::PubUpgrade => vec!["pub".into(), "upgrade".into()],
            Self::PubOutdated => vec!["pub".into(), "outdated".into()],
            Self::Clean => vec!["clean".into()],
            Self::Doctor => vec!["doctor".into(), "-v".into()],
            Self::BuildRunnerBuild => match project.kind {
                crate::project::ProjectKind::Flutter => vec![
                    "pub".into(),
                    "run".into(),
                    "build_runner".into(),
                    "build".into(),
                    "--delete-conflicting-outputs".into(),
                ],
                crate::project::ProjectKind::Dart => vec![
                    "run".into(),
                    "build_runner".into(),
                    "build".into(),
                    "--delete-conflicting-outputs".into(),
                ],
            },
            Self::Screenshot {
                device_id,
                output_path,
            } => {
                let mut args = vec!["screenshot".into(), "-o".into(), output_path.clone()];
                if let Some(dev) = device_id
                    && !dev.trim().is_empty()
                {
                    args.push("-d".into());
                    args.push(dev.clone());
                }
                args
            }
            Self::TestFile {
                relative_path,
                test_name,
            } => {
                let mut args = vec!["test".into(), relative_path.clone()];
                if let Some(name) = test_name
                    && !name.trim().is_empty()
                {
                    args.push("--name".into());
                    args.push(name.clone());
                }
                args
            }
        }
    }
}

/// Output captured from executing a whitelisted CLI command.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub log_path: Option<PathBuf>,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("Another command is already running for project `{0}`")]
    AlreadyRunning(String),
    #[error("Command `{0}` is only available for Flutter projects")]
    FlutterOnly(String),
    #[error("Failed to spawn `{program}`: {source}")]
    SpawnFailed {
        program: String,
        #[source]
        source: std::io::Error,
    },
}

/// Non-blocking CLI runner with per-project re-entrancy protection.
#[derive(Debug, Clone, Default)]
pub struct CommandRunner {
    running_projects: Arc<DashSet<PathBuf>>,
}

impl CommandRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn run_project_command(
        &self,
        project: &ProjectInfo,
        cmd: &AllowedCommand,
        custom_sdk_path: Option<&str>,
    ) -> Result<CommandOutput, RunnerError> {
        if matches!(cmd, AllowedCommand::Clean | AllowedCommand::Screenshot { .. })
            && project.kind != crate::project::ProjectKind::Flutter
        {
            return Err(RunnerError::FlutterOnly(cmd.display_name().to_string()));
        }

        let lock_key = project.root.clone();
        if !self.running_projects.insert(lock_key.clone()) {
            return Err(RunnerError::AlreadyRunning(
                project.root.display().to_string(),
            ));
        }

        let _guard = RunningGuard {
            set: Arc::clone(&self.running_projects),
            key: lock_key,
        };

        let program = resolve_sdk_executable(&project.root, project.kind, custom_sdk_path);
        let args = cmd.build_args(project);
        let command_line = format!("{program} {}", args.join(" "));

        let output = spawn_cli(&program, &args, &project.root)
            .await
            .map_err(|source| RunnerError::SpawnFailed {
                program: program.clone(),
                source,
            })?;

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = strip_ansi_codes(&String::from_utf8_lossy(&output.stdout));
        let stderr = strip_ansi_codes(&String::from_utf8_lossy(&output.stderr));

        let log_path = if exit_code != 0 || cmd.always_open_log() {
            write_command_log(
                cmd.display_name(),
                &command_line,
                &project.root,
                exit_code,
                &stderr,
                &stdout,
            )
            .ok()
        } else {
            None
        };

        Ok(CommandOutput {
            exit_code,
            stdout,
            stderr,
            log_path,
        })
    }

    /// Runs `flutter devices --machine` without holding a per-project lock.
    pub async fn run_flutter_devices(
        &self,
        cwd: &Path,
        custom_sdk_path: Option<&str>,
    ) -> Result<CommandOutput, RunnerError> {
        let program =
            resolve_sdk_executable(cwd, crate::project::ProjectKind::Flutter, custom_sdk_path);
        let args = vec!["devices".to_string(), "--machine".to_string()];
        let command_line = format!("{program} {}", args.join(" "));

        let output = spawn_cli(&program, &args, cwd)
            .await
            .map_err(|source| RunnerError::SpawnFailed {
                program: program.clone(),
                source,
            })?;

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = strip_ansi_codes(&String::from_utf8_lossy(&output.stdout));
        let stderr = strip_ansi_codes(&String::from_utf8_lossy(&output.stderr));

        let log_path = if exit_code != 0 {
            write_command_log(
                "flutter-devices",
                &command_line,
                cwd,
                exit_code,
                &stderr,
                &stdout,
            )
            .ok()
        } else {
            None
        };

        Ok(CommandOutput {
            exit_code,
            stdout,
            stderr,
            log_path,
        })
    }
}

struct RunningGuard {
    set: Arc<DashSet<PathBuf>>,
    key: PathBuf,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.set.remove(&self.key);
    }
}

async fn spawn_cli(
    program: &str,
    args: &[String],
    cwd: &Path,
) -> std::io::Result<std::process::Output> {
    #[cfg(windows)]
    {
        let lower = program.to_ascii_lowercase();
        if lower.ends_with(".bat") || lower.ends_with(".cmd") {
            let mut command = Command::new("cmd");
            command.arg("/C").arg(program).args(args).current_dir(cwd);
            return command.output().await;
        }
    }

    let mut command = Command::new(program);
    command.args(args).current_dir(cwd);
    command.output().await
}

/// Strips ANSI CSI and OSC escape codes from CLI output.
pub fn strip_ansi_codes(input: &str) -> String {
    static ANSI_RE: OnceLock<Regex> = OnceLock::new();
    let re = ANSI_RE.get_or_init(|| {
        Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)")
            .expect("valid ANSI regex")
    });
    re.replace_all(input, "").into_owned()
}

/// Writes full command execution diagnostics to a temporary `.log` file.
pub fn write_command_log(
    label: &str,
    command_line: &str,
    cwd: &Path,
    exit_code: i32,
    stderr: &str,
    stdout: &str,
) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join("flutter-debugger-logs");
    fs::create_dir_all(&dir)?;

    let safe_label: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let filename = format!("{safe_label}-{}.log", now_unix());
    let path = dir.join(filename);

    let body = format!(
        "Command: {command_line}\nWorking Directory: {}\nExit Code: {exit_code}\n\n=== STDERR ===\n{}\n\n=== STDOUT ===\n{}\n",
        cwd.display(),
        if stderr.trim().is_empty() {
            "(empty)"
        } else {
            stderr.trim_end()
        },
        if stdout.trim().is_empty() {
            "(empty)"
        } else {
            stdout.trim_end()
        }
    );

    fs::write(&path, body)?;
    Ok(path)
}

/// Extracts a concise, high-signal error summary from full stderr/stdout rather than
/// blindly taking only the last line.
pub fn extract_error_summary(stderr: &str, stdout: &str, exit_code: i32) -> String {
    let combined = if !stderr.trim().is_empty() {
        stderr
    } else {
        stdout
    };

    let priority_keywords = [
        "Error:",
        "Exception:",
        "Because ",
        "version solving failed",
        "Could not find",
        "Failed",
        "pub failed",
        "Some tests failed",
    ];

    let mut highlights: Vec<&str> = Vec::new();
    for line in combined.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if priority_keywords.iter().any(|kw| line.contains(kw)) {
            highlights.push(line);
            if highlights.len() >= 3 {
                break;
            }
        }
    }

    if !highlights.is_empty() {
        return highlights.join(" | ");
    }

    let fallback: Vec<&str> = combined
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(3)
        .collect();

    if fallback.is_empty() {
        format!("Command exited with code {exit_code}")
    } else {
        fallback.join(" | ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_color_and_osc_codes() {
        let raw = "\x1b[31mError:\x1b[0m version solving failed\x1b]0;title\x07";
        assert_eq!(strip_ansi_codes(raw), "Error: version solving failed");
    }

    #[test]
    fn extracts_meaningful_error_not_only_last_line() {
        let stderr = "Resolving dependencies...\nBecause app depends on foo ^9.0.0 which doesn't match any versions, version solving failed.\n\nYou can try the following suggestion to make the pubspec resolve:\n* Try upgrading your constraint.";
        let summary = extract_error_summary(stderr, "", 1);
        assert!(summary.contains("version solving failed"));
        assert!(!summary.starts_with("* Try upgrading"));
    }

    #[test]
    fn writes_complete_log_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_command_log(
            "pub get",
            "flutter pub get",
            dir.path(),
            1,
            "Error: offline",
            "Resolving...",
        )
        .expect("log written");
        let content = fs::read_to_string(&path).expect("read log");
        assert!(content.contains("Command: flutter pub get"));
        assert!(content.contains("Exit Code: 1"));
        assert!(content.contains("Error: offline"));
        assert!(content.contains("Resolving..."));
        let _ = fs::remove_file(path);
    }
}
