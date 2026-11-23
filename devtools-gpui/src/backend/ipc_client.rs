use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::backend::models::ServiceOverlay;
use crate::backend::session::{ActiveSessionRecord, is_pid_alive};

/// Loopback TCP NDJSON protocol compatible with `server/src/ipc.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase")]
pub enum IpcRequest {
    HotReload,
    HotRestart,
    Stop,
    Status,
    ToggleOverlay {
        overlay: ServiceOverlay,
    },
    CallServiceExtension {
        method: String,
        params: Value,
    },
    OpenDevTools {
        #[serde(default)]
        page: Option<String>,
    },
    Screenshot {
        #[serde(default)]
        output_path: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    pub message: String,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub vm_service_uri: Option<String>,
    #[serde(default)]
    pub devtools_uri: Option<String>,
}

pub async fn send_ipc_to_session(
    record: &ActiveSessionRecord,
    request: &IpcRequest,
) -> Result<IpcResponse, String> {
    if !is_pid_alive(record.pid) {
        return Err("Session process is no longer alive.".to_string());
    }

    let addr = format!("127.0.0.1:{}", record.ipc_port);
    let mut stream = tokio::time::timeout(Duration::from_millis(400), TcpStream::connect(&addr))
        .await
        .map_err(|_| format!("Timed out connecting to session IPC at {addr}"))?
        .map_err(|e| format!("Failed to connect to session IPC at {addr}: {e}"))?;

    let mut payload = serde_json::to_string(request).map_err(|e| e.to_string())?;
    payload.push('\n');
    stream
        .write_all(payload.as_bytes())
        .await
        .map_err(|e| format!("Failed to write IPC request: {e}"))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let n = tokio::time::timeout(Duration::from_secs(15), reader.read_line(&mut line))
        .await
        .map_err(|_| "Timed out waiting for IPC response".to_string())?
        .map_err(|e| format!("Failed to read IPC response: {e}"))?;

    if n == 0 {
        return Err("Session IPC closed connection without response.".to_string());
    }

    serde_json::from_str::<IpcResponse>(line.trim())
        .map_err(|e| format!("Invalid IPC JSON response: {e}"))
}
