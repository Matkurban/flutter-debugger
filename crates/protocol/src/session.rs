use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Persistent metadata for a single active Flutter/Dart session (DAP or `ctl run`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveSessionRecord {
    pub session_id: String,
    pub project_root: String,
    #[serde(default)]
    pub device_id: Option<String>,
    pub mode: String,
    pub pid: u32,
    #[serde(alias = "port")]
    pub ipc_port: u16,
    #[serde(default)]
    pub vm_service_uri: Option<String>,
    #[serde(default)]
    pub devtools_uri: Option<String>,
    #[serde(default)]
    pub started_at: u64,
    #[serde(default)]
    pub is_test: bool,
    #[serde(default = "default_session_kind")]
    pub kind: String,
}

fn default_session_kind() -> String {
    "dap".to_string()
}

impl ActiveSessionRecord {
    pub fn project_name(&self) -> String {
        Path::new(&self.project_root)
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.project_root)
            .to_string()
    }

    pub fn display_label(&self) -> String {
        let dev = self.device_id.as_deref().unwrap_or("device");
        format!("{} ({} · {})", self.project_name(), dev, self.mode)
    }
}

pub fn default_cache_dir() -> PathBuf {
    let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
    base.join("flutter-debugger")
}

pub fn default_sessions_dir() -> PathBuf {
    default_cache_dir().join("sessions")
}

/// Lists all live active sessions under `sessions_dir`, automatically pruning dead PIDs
/// and corrupt session files. Sorted newest-first (`started_at` descending).
pub fn list_active_sessions_in(sessions_dir: &Path) -> Vec<ActiveSessionRecord> {
    let Ok(entries) = fs::read_dir(sessions_dir) else {
        return Vec::new();
    };

    let mut active = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            let _ = fs::remove_file(&path);
            continue;
        };
        let Ok(record) = serde_json::from_slice::<ActiveSessionRecord>(&bytes) else {
            let _ = fs::remove_file(&path);
            continue;
        };
        if !is_pid_alive(record.pid) {
            let _ = fs::remove_file(&path);
            continue;
        }
        active.push(record);
    }

    active.sort_by_key(|a| std::cmp::Reverse(a.started_at));
    active
}

pub fn select_best_session<'a>(
    sessions: &'a [ActiveSessionRecord],
    preferred_session_id: Option<&str>,
    preferred_project: Option<&str>,
) -> Option<&'a ActiveSessionRecord> {
    if let Some(sid) = preferred_session_id
        && let Some(found) = sessions.iter().find(|s| s.session_id == sid)
    {
        return Some(found);
    }

    if let Some(proj) = preferred_project {
        let normalized = normalize_project_key(Path::new(proj));
        if let Some(found) = sessions
            .iter()
            .find(|s| normalize_project_key(Path::new(&s.project_root)) == normalized)
        {
            return Some(found);
        }
    }

    sessions.first()
}

pub fn normalize_project_key(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Fast OS-level PID liveness check (`kill(pid, 0)` on Unix, `OpenProcess` + `GetExitCodeProcess` on Windows).
pub fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        let Ok(pid_i32) = i32::try_from(pid) else {
            return false;
        };
        let res = unsafe { kill(pid_i32, 0) };
        if res == 0 {
            return true;
        }
        matches!(std::io::Error::last_os_error().raw_os_error(), Some(1))
    }

    #[cfg(windows)]
    {
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        const STILL_ACTIVE: u32 = 259;
        const ERROR_ACCESS_DENIED: i32 = 5;

        unsafe extern "system" {
            fn OpenProcess(
                dw_desired_access: u32,
                b_inherit_handle: i32,
                dw_process_id: u32,
            ) -> *mut std::ffi::c_void;
            fn GetExitCodeProcess(
                h_process: *mut std::ffi::c_void,
                lp_exit_code: *mut u32,
            ) -> i32;
            fn CloseHandle(h_object: *mut std::ffi::c_void) -> i32;
        }

        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return matches!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(ERROR_ACCESS_DENIED)
            );
        }
        let mut exit_code: u32 = 0;
        let ok = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
        unsafe {
            CloseHandle(handle);
        }
        ok != 0 && exit_code == STILL_ACTIVE
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_dead_pids_and_sorts_sessions_newest_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions_dir = dir.path().join("sessions");
        fs::create_dir_all(&sessions_dir).expect("mkdir");

        let alive_pid = std::process::id();
        let dead_pid = 999_999_999u32;

        let older = ActiveSessionRecord {
            session_id: "s-older".into(),
            project_root: "/workspace/app_one".into(),
            device_id: Some("macos".into()),
            mode: "debug".into(),
            pid: alive_pid,
            ipc_port: 41001,
            vm_service_uri: Some("ws://127.0.0.1:50001/ws".into()),
            devtools_uri: None,
            started_at: 1000,
            is_test: false,
            kind: "dap".into(),
        };
        let newer = ActiveSessionRecord {
            session_id: "s-newer".into(),
            project_root: "/workspace/app_two".into(),
            device_id: Some("iphone".into()),
            mode: "profile".into(),
            pid: alive_pid,
            ipc_port: 41002,
            vm_service_uri: Some("ws://127.0.0.1:50002/ws".into()),
            devtools_uri: None,
            started_at: 2000,
            is_test: false,
            kind: "run".into(),
        };
        let dead = ActiveSessionRecord {
            session_id: "s-dead".into(),
            project_root: "/workspace/app_dead".into(),
            device_id: None,
            mode: "debug".into(),
            pid: dead_pid,
            ipc_port: 41003,
            vm_service_uri: None,
            devtools_uri: None,
            started_at: 3000,
            is_test: false,
            kind: "dap".into(),
        };

        fs::write(
            sessions_dir.join("s-older.json"),
            serde_json::to_vec(&older).unwrap(),
        )
        .unwrap();
        fs::write(
            sessions_dir.join("s-newer.json"),
            serde_json::to_vec(&newer).unwrap(),
        )
        .unwrap();
        let dead_file = sessions_dir.join("s-dead.json");
        fs::write(&dead_file, serde_json::to_vec(&dead).unwrap()).unwrap();

        let list = list_active_sessions_in(&sessions_dir);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].session_id, "s-newer");
        assert_eq!(list[1].session_id, "s-older");
        assert!(!dead_file.exists());

        let picked = select_best_session(&list, None, Some("/workspace/app_one")).expect("pick");
        assert_eq!(picked.session_id, "s-older");
    }
}
