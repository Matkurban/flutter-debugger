use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum DevToolsError {
    #[error("Connection error: {0}")]
    Connection(String),

    #[error("VM Service RPC `{method}` failed: {message}")]
    Rpc { method: String, message: String },

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("IPC error: {0}")]
    Ipc(String),

    #[error("IO error: {0}")]
    Io(String),
}
