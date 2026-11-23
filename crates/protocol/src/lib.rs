#![cfg_attr(not(test), deny(clippy::unwrap_used))]

pub mod ipc;
pub mod overlay;
pub mod session;

pub use ipc::{DebugLibraryScope, ExceptionPauseMode, IpcRequest, IpcResponse};
pub use overlay::{OverlayState, ServiceOverlay};
pub use session::{
    ActiveSessionRecord, default_cache_dir, default_sessions_dir, is_pid_alive,
    list_active_sessions_in, normalize_project_key, select_best_session,
};
