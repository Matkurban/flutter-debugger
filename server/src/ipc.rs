use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::cache::{DiskCache, normalize_project_key};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveSessionRecord {
    pub project_root: String,
    pub port: u16,
    pub mode: String,
    pub vm_service_uri: Option<String>,
    pub devtools_uri: Option<String>,
    pub pid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase")]
pub enum IpcRequest {
    HotReload,
    HotRestart,
    Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    pub message: String,
    pub mode: Option<String>,
    pub vm_service_uri: Option<String>,
    pub devtools_uri: Option<String>,
}

pub fn session_file_path(cache: &DiskCache, project_root: &Path) -> PathBuf {
    let key = normalize_project_key(project_root);
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    let hash = hasher.finish();
    cache
        .root_dir()
        .join("sessions")
        .join(format!("{hash:016x}.json"))
}

pub fn write_session_record(
    cache: &DiskCache,
    project_root: &Path,
    record: &ActiveSessionRecord,
) -> std::io::Result<PathBuf> {
    let path = session_file_path(cache, project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_vec_pretty(record)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, &path)?;
    Ok(path)
}

pub fn read_session_record(cache: &DiskCache, project_root: &Path) -> Option<ActiveSessionRecord> {
    let path = session_file_path(cache, project_root);
    let bytes = fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn remove_session_record(cache: &DiskCache, project_root: &Path) {
    let path = session_file_path(cache, project_root);
    let _ = fs::remove_file(path);
}

/// Sends an IPC command to an active DAP proxy session on loopback TCP.
///
/// Automatically cleans up stale session files if the DAP process is no longer reachable.
pub async fn send_ipc_request(
    cache: &DiskCache,
    project_root: &Path,
    request: &IpcRequest,
) -> Option<IpcResponse> {
    let record = read_session_record(cache, project_root)?;
    let addr = format!("127.0.0.1:{}", record.port);

    let Ok(Ok(mut stream)) = tokio::time::timeout(
        Duration::from_millis(800),
        TcpStream::connect(&addr),
    )
    .await
    else {
        remove_session_record(cache, project_root);
        return None;
    };

    let mut payload = serde_json::to_string(request).ok()?;
    payload.push('\n');
    if stream.write_all(payload.as_bytes()).await.is_err() {
        return None;
    }

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    match tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line)).await {
        Ok(Ok(n)) if n > 0 => serde_json::from_str::<IpcResponse>(line.trim()).ok(),
        _ => None,
    }
}
