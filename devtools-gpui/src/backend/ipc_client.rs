use std::time::Duration;

use flutter_debugger_protocol::{ActiveSessionRecord, IpcRequest, IpcResponse, is_pid_alive};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::backend::error::DevToolsError;

pub async fn send_ipc_to_session(
    record: &ActiveSessionRecord,
    request: &IpcRequest,
) -> Result<IpcResponse, DevToolsError> {
    if !is_pid_alive(record.pid) {
        return Err(DevToolsError::Ipc(
            "Session process is no longer alive.".to_string(),
        ));
    }

    let addr = format!("127.0.0.1:{}", record.ipc_port);
    let mut stream = tokio::time::timeout(Duration::from_millis(400), TcpStream::connect(&addr))
        .await
        .map_err(|_| DevToolsError::Ipc(format!("Timed out connecting to session IPC at {addr}")))?
        .map_err(|e| DevToolsError::Ipc(format!("Failed to connect to session IPC at {addr}: {e}")))?;

    let mut payload =
        serde_json::to_string(request).map_err(|e| DevToolsError::Ipc(e.to_string()))?;
    payload.push('\n');
    stream
        .write_all(payload.as_bytes())
        .await
        .map_err(|e| DevToolsError::Ipc(format!("Failed to write IPC request: {e}")))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let n = tokio::time::timeout(Duration::from_secs(15), reader.read_line(&mut line))
        .await
        .map_err(|_| DevToolsError::Ipc("Timed out waiting for IPC response".to_string()))?
        .map_err(|e| DevToolsError::Ipc(format!("Failed to read IPC response: {e}")))?;

    if n == 0 {
        return Err(DevToolsError::Ipc(
            "Session IPC closed connection without response.".to_string(),
        ));
    }

    serde_json::from_str::<IpcResponse>(line.trim())
        .map_err(|e| DevToolsError::Ipc(format!("Invalid IPC JSON response: {e}")))
}
