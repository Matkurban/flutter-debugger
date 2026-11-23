use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_channel::Receiver;
use serde::{Deserialize, Serialize};

use crate::backend::session::is_pid_alive;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SingleInstanceRecord {
    pub pid: u32,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SingleInstanceFocusCommand {
    pub action: String,
    #[serde(default)]
    pub page: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub vm_uri: Option<String>,
}

pub fn default_instance_lock_path() -> PathBuf {
    let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
    base.join("flutter-devtools-gpui.json")
}

/// Checks if a live instance is already running at `lock_path`.
/// If alive and reachable via TCP, sends `cmd` and returns `Ok(None)`.
/// Otherwise binds a new control port, writes `lock_path`, spawns a listener thread,
/// and returns `Ok(Some(receiver))`.
pub fn try_forward_or_bind_single_instance(
    cmd: &SingleInstanceFocusCommand,
) -> Result<Option<Receiver<SingleInstanceFocusCommand>>, String> {
    try_forward_or_bind_with_path(&default_instance_lock_path(), cmd)
}

pub fn try_forward_or_bind_with_path(
    lock_path: &Path,
    cmd: &SingleInstanceFocusCommand,
) -> Result<Option<Receiver<SingleInstanceFocusCommand>>, String> {
    if let Ok(bytes) = fs::read(lock_path)
        && let Ok(record) = serde_json::from_slice::<SingleInstanceRecord>(&bytes)
    {
        if is_pid_alive(record.pid) && record.pid != std::process::id() {
            let addr = format!("127.0.0.1:{}", record.port);
            if let Ok(addr_parsed) = addr.parse()
                && let Ok(mut stream) =
                    TcpStream::connect_timeout(&addr_parsed, Duration::from_millis(250))
            {
                let mut payload = serde_json::to_string(cmd).map_err(|e| e.to_string())?;
                payload.push('\n');
                if stream.write_all(payload.as_bytes()).is_ok() {
                    let _ = stream.flush();
                    return Ok(None);
                }
            }
        }
        let _ = fs::remove_file(lock_path);
    }

    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("bind single-instance port: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("local_addr: {e}"))?
        .port();

    if let Some(parent) = lock_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let rec = SingleInstanceRecord {
        pid: std::process::id(),
        port,
    };
    if let Ok(data) = serde_json::to_vec_pretty(&rec) {
        let tmp = lock_path.with_extension("json.tmp");
        if fs::write(&tmp, data).is_ok() {
            let _ = fs::rename(&tmp, lock_path);
        }
    }

    let (tx, rx) = async_channel::unbounded();
    std::thread::Builder::new()
        .name("devtools-single-instance".to_string())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) > 0
                    && let Ok(focus_cmd) =
                        serde_json::from_str::<SingleInstanceFocusCommand>(line.trim())
                {
                    let _ = tx.send_blocking(focus_cmd);
                }
            }
        })
        .map_err(|e| format!("spawn single-instance listener: {e}"))?;

    Ok(Some(rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_instance_binds_and_forwards_focus_command() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("flutter-devtools-gpui.json");

        // Write a dead PID first to verify stale lock cleanup
        let stale = SingleInstanceRecord {
            pid: 999_999_999,
            port: 65000,
        };
        fs::write(&lock_path, serde_json::to_vec(&stale).unwrap()).unwrap();

        let initial_cmd = SingleInstanceFocusCommand {
            action: "focus".into(),
            page: Some("network".into()),
            project: None,
            session_id: None,
            vm_uri: None,
        };

        let rx = try_forward_or_bind_with_path(&lock_path, &initial_cmd)
            .expect("bind ok")
            .expect("should become primary instance");

        // Now simulate a second process reading the lock file and connecting to its port
        let saved: SingleInstanceRecord =
            serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
        assert_eq!(saved.pid, std::process::id());

        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", saved.port)).unwrap();
        let second_cmd = SingleInstanceFocusCommand {
            action: "focus".into(),
            page: Some("inspector".into()),
            project: Some("/tmp/my_app".into()),
            session_id: Some("s-123".into()),
            vm_uri: None,
        };
        writeln!(stream, "{}", serde_json::to_string(&second_cmd).unwrap()).unwrap();

        let received = rx.recv_blocking().expect("receive focus command");
        assert_eq!(received.page.as_deref(), Some("inspector"));
        assert_eq!(received.project.as_deref(), Some("/tmp/my_app"));
        assert_eq!(received.session_id.as_deref(), Some("s-123"));
    }
}
