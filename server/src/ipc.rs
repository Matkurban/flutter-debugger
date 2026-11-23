use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::cache::{DiskCache, normalize_project_key, now_unix};

pub use flutter_debugger_protocol::{
    ActiveSessionRecord, IpcRequest, IpcResponse, OverlayState, ServiceOverlay, is_pid_alive,
    list_active_sessions_in,
};

pub fn generate_session_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("s-{}-{}-{seq}", std::process::id(), now_unix())
}

pub fn sessions_dir(cache: &DiskCache) -> PathBuf {
    cache.root_dir().join("sessions")
}

pub fn session_file_path_by_id(cache: &DiskCache, session_id: &str) -> PathBuf {
    sessions_dir(cache).join(format!("{session_id}.json"))
}

pub fn write_session_record(
    cache: &DiskCache,
    record: &ActiveSessionRecord,
) -> std::io::Result<PathBuf> {
    let dir = sessions_dir(cache);
    fs::create_dir_all(&dir)?;
    let path = session_file_path_by_id(cache, &record.session_id);
    let data = serde_json::to_vec_pretty(record)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, &path)?;
    Ok(path)
}

pub fn remove_session_record_by_id(cache: &DiskCache, session_id: &str) {
    let path = session_file_path_by_id(cache, session_id);
    let _ = fs::remove_file(path);
}

/// Lists all live active sessions across all projects, automatically pruning dead PIDs
/// and corrupt/legacy session files. Sorted newest-first (`started_at` descending).
pub fn list_active_sessions(cache: &DiskCache) -> Vec<ActiveSessionRecord> {
    list_active_sessions_in(&sessions_dir(cache))
}

/// Returns all live sessions matching `project_root` (newest first).
pub fn find_sessions_for_project(
    cache: &DiskCache,
    project_root: &Path,
) -> Vec<ActiveSessionRecord> {
    let key = normalize_project_key(project_root);
    list_active_sessions(cache)
        .into_iter()
        .filter(|s| s.project_root == key)
        .collect()
}

/// Returns the newest live session for `project_root` (or globally if `project_root` is `None`).
pub fn read_session_record(cache: &DiskCache, project_root: &Path) -> Option<ActiveSessionRecord> {
    find_sessions_for_project(cache, project_root)
        .into_iter()
        .next()
}

/// Returns the newest live session for `project_root`, falling back to any live session
/// if `project_root` is `None`.
pub fn find_latest_session(
    cache: &DiskCache,
    project_root: Option<&Path>,
) -> Option<ActiveSessionRecord> {
    match project_root {
        Some(root) => read_session_record(cache, root),
        None => list_active_sessions(cache).into_iter().next(),
    }
}

/// Sends an IPC command to a specific active session over loopback TCP.
/// Checks PID liveness first and prunes stale session files immediately.
pub async fn send_ipc_to_session(
    cache: &DiskCache,
    record: &ActiveSessionRecord,
    request: &IpcRequest,
) -> Option<IpcResponse> {
    if !is_pid_alive(record.pid) {
        remove_session_record_by_id(cache, &record.session_id);
        return None;
    }

    let addr = format!("127.0.0.1:{}", record.ipc_port);
    let Ok(Ok(mut stream)) =
        tokio::time::timeout(Duration::from_millis(300), TcpStream::connect(&addr)).await
    else {
        remove_session_record_by_id(cache, &record.session_id);
        return None;
    };

    let mut payload = serde_json::to_string(request).ok()?;
    payload.push('\n');
    if stream.write_all(payload.as_bytes()).await.is_err() {
        return None;
    }

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    match tokio::time::timeout(Duration::from_secs(15), reader.read_line(&mut line)).await {
        Ok(Ok(n)) if n > 0 => serde_json::from_str::<IpcResponse>(line.trim()).ok(),
        _ => None,
    }
}

/// Sends an IPC command to the newest live session for `project_root`.
pub async fn send_ipc_request(
    cache: &DiskCache,
    project_root: &Path,
    request: &IpcRequest,
) -> Option<IpcResponse> {
    let sessions = find_sessions_for_project(cache, project_root);
    for record in sessions {
        if let Some(resp) = send_ipc_to_session(cache, &record, request).await {
            return Some(resp);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_session_registry_and_dead_pid_cleanup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::with_root(dir.path().to_path_buf());
        let project_a = dir.path().join("app_a");
        let project_b = dir.path().join("app_b");
        fs::create_dir_all(&project_a).expect("mkdir a");
        fs::create_dir_all(&project_b).expect("mkdir b");

        let alive_pid = std::process::id();
        let dead_pid = 999_999_999u32;

        let rec_a1 = ActiveSessionRecord {
            session_id: "sess-a1".into(),
            project_root: normalize_project_key(&project_a),
            device_id: Some("macos".into()),
            mode: "debug".into(),
            pid: alive_pid,
            ipc_port: 40001,
            vm_service_uri: Some("ws://127.0.0.1:50001/ws".into()),
            devtools_uri: None,
            started_at: 100,
            is_test: false,
            kind: "dap".into(),
        };
        let rec_a2 = ActiveSessionRecord {
            session_id: "sess-a2".into(),
            project_root: normalize_project_key(&project_a),
            device_id: Some("ios".into()),
            mode: "profile".into(),
            pid: alive_pid,
            ipc_port: 40002,
            vm_service_uri: None,
            devtools_uri: None,
            started_at: 200,
            is_test: false,
            kind: "run".into(),
        };
        let rec_dead = ActiveSessionRecord {
            session_id: "sess-dead".into(),
            project_root: normalize_project_key(&project_b),
            device_id: None,
            mode: "debug".into(),
            pid: dead_pid,
            ipc_port: 40003,
            vm_service_uri: None,
            devtools_uri: None,
            started_at: 300,
            is_test: false,
            kind: "dap".into(),
        };

        write_session_record(&cache, &rec_a1).expect("write a1");
        write_session_record(&cache, &rec_a2).expect("write a2");
        write_session_record(&cache, &rec_dead).expect("write dead");

        let all = list_active_sessions(&cache);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].session_id, "sess-a2");
        assert_eq!(all[1].session_id, "sess-a1");
        assert!(!session_file_path_by_id(&cache, "sess-dead").exists());

        let latest_a = read_session_record(&cache, &project_a).expect("latest a");
        assert_eq!(latest_a.session_id, "sess-a2");
    }

    #[test]
    fn overlay_state_toggles_all_extensions() {
        let mut state = OverlayState::default();
        let (method, params, label) = state.toggle(ServiceOverlay::Inspector);
        assert_eq!(method, "ext.flutter.inspector.show");
        assert_eq!(params["enabled"], "true");
        assert!(label.contains("ON"));

        let (method, params, label) = state.toggle(ServiceOverlay::BrightnessOverride);
        assert_eq!(method, "ext.flutter.brightnessOverride");
        assert_eq!(params["value"], "Brightness.dark");
        assert!(label.contains("Dark"));

        let (_, params2, label2) = state.toggle(ServiceOverlay::BrightnessOverride);
        assert_eq!(params2["value"], "Brightness.light");
        assert!(label2.contains("Light"));
    }

    #[test]
    fn overlay_state_preview_does_not_mutate_until_committed() {
        let mut state = OverlayState::default();
        assert!(!state.inspector);

        let (method, params, label) = state.preview_toggle(ServiceOverlay::Inspector);
        assert_eq!(method, "ext.flutter.inspector.show");
        assert_eq!(params["enabled"], "true");
        assert!(label.contains("ON"));
        assert!(!state.inspector);

        let (m2, p2, l2) = state.toggle(ServiceOverlay::Inspector);
        assert_eq!(m2, method);
        assert_eq!(p2, params);
        assert_eq!(l2, label);
        assert!(state.inspector);
    }

    #[test]
    fn parses_service_overlay_aliases() {
        assert_eq!(
            ServiceOverlay::parse("inspector"),
            Some(ServiceOverlay::Inspector)
        );
        assert_eq!(
            ServiceOverlay::parse("debug-paint"),
            Some(ServiceOverlay::DebugPaint)
        );
        assert_eq!(
            ServiceOverlay::parse("slow-anim"),
            Some(ServiceOverlay::SlowAnimations)
        );
        assert_eq!(
            ServiceOverlay::parse("brightness"),
            Some(ServiceOverlay::BrightnessOverride)
        );
        assert_eq!(
            ServiceOverlay::parse("oversized"),
            Some(ServiceOverlay::OversizedImages)
        );
        assert_eq!(ServiceOverlay::parse("unknown"), None);
    }
}
