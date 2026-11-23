use std::path::Path;
use std::process::Stdio;

use tokio::process::{Child, Command};

/// Builds a cross-platform `tokio::process::Command` that transparently wraps
/// Windows `.bat`/`.cmd` shims (`flutter.bat`, `dart.bat`) via `cmd /C`.
pub fn build_command<I, S>(executable: &str, args: I, cwd: &Path) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    #[cfg(windows)]
    {
        let lower = executable.to_ascii_lowercase();
        if lower.ends_with(".bat") || lower.ends_with(".cmd") {
            let mut cmd = Command::new("cmd");
            cmd.arg("/C").arg(executable).args(args).current_dir(cwd);
            return cmd;
        }
    }

    let mut cmd = Command::new(executable);
    cmd.args(args).current_dir(cwd);
    cmd
}

/// Reliably terminates a child process and all of its descendants across Unix and Windows.
pub async fn terminate_child_tree(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let _ = Command::new("pkill")
            .args(["-TERM", "-P", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }

    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }

    let _ = child.kill().await;
    let _ = child.wait().await;
}
