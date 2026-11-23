use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::cache::DiskCache;
use crate::ipc::is_pid_alive;
use crate::project::{ProjectKind, resolve_sdk_executable};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DevToolsServerRecord {
    pub pid: u32,
    pub host: String,
    pub port: u16,
}

impl DevToolsServerRecord {
    fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

pub fn normalize_devtools_page(page: Option<&str>) -> Option<&'static str> {
    match page.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        None | Some("" | "home" | "default") => None,
        Some("inspector" | "widget" | "widgets") => Some("inspector"),
        Some("performance" | "perf") => Some("performance"),
        Some("memory" | "mem") => Some("memory"),
        Some("network" | "net") => Some("network"),
        Some("logging" | "log" | "logs") => Some("logging"),
        Some("cpu" | "cpu-profiler" | "profiler") => Some("cpu-profiler"),
        Some("debugger" | "debug") => Some("debugger"),
        Some("app-size" | "size") => Some("app-size"),
        _ => None,
    }
}

pub fn build_devtools_url(
    devtools_uri: Option<&str>,
    vm_service_uri: Option<&str>,
    page: Option<&str>,
) -> Option<String> {
    let route = normalize_devtools_page(page);

    if let Some(dt) = devtools_uri.map(str::trim).filter(|s| !s.is_empty()) {
        let mut base = dt.to_string();
        if let Some(hash_idx) = base.find('#') {
            base.truncate(hash_idx);
        }
        let (path_part, existing_query) = match base.split_once('?') {
            Some((p, q)) => (p.trim_end_matches('/'), Some(q)),
            None => (base.trim_end_matches('/'), None),
        };

        let query = if let Some(q) = existing_query {
            q.to_string()
        } else if let Some(vm) = vm_service_uri {
            let mut ser = url::form_urlencoded::Serializer::new(String::new());
            ser.append_pair("uri", vm);
            ser.finish()
        } else {
            String::new()
        };

        return Some(match (route, query.is_empty()) {
            (Some(r), false) => format!("{path_part}/#/{r}?{query}"),
            (Some(r), true) => format!("{path_part}/#/{r}"),
            (None, false) => format!("{path_part}/?{query}"),
            (None, true) => format!("{path_part}/"),
        });
    }

    if let Some(vm) = vm_service_uri {
        let parsed = url::Url::parse(vm).ok()?;
        let host = parsed.host_str()?;
        let port = parsed.port()?;
        let path = parsed.path().trim_end_matches("/ws").trim_end_matches('/');
        let mut ser = url::form_urlencoded::Serializer::new(String::new());
        ser.append_pair("uri", vm);
        let query = ser.finish();
        return Some(match route {
            Some(r) => format!("http://{host}:{port}{path}/devtools/#/{r}?{query}"),
            None => format!("http://{host}:{port}{path}/devtools/?{query}"),
        });
    }

    None
}

pub async fn resolve_or_start_devtools_url(
    cache: &DiskCache,
    project_root: &Path,
    devtools_uri: Option<&str>,
    vm_service_uri: Option<&str>,
    page: Option<&str>,
    custom_sdk_path: Option<&str>,
) -> Option<String> {
    if devtools_uri.is_some() {
        return build_devtools_url(devtools_uri, vm_service_uri, page);
    }

    let vm = vm_service_uri?;

    let record_path = cache.root_dir().join("devtools.json");
    if let Ok(bytes) = fs::read(&record_path)
        && let Ok(rec) = serde_json::from_slice::<DevToolsServerRecord>(&bytes)
        && is_pid_alive(rec.pid)
    {
        return build_devtools_url(Some(&rec.base_url()), Some(vm), page);
    }

    if let Some(rec) = spawn_devtools_server(project_root, custom_sdk_path).await {
        if let Some(parent) = record_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_vec_pretty(&rec) {
            let _ = fs::write(&record_path, data);
        }
        return build_devtools_url(Some(&rec.base_url()), Some(vm), page);
    }

    build_devtools_url(None, Some(vm), page)
}

async fn spawn_devtools_server(
    project_root: &Path,
    custom_sdk_path: Option<&str>,
) -> Option<DevToolsServerRecord> {
    let dart_exe = resolve_sdk_executable(project_root, ProjectKind::Dart, custom_sdk_path);
    let mut child = Command::new(&dart_exe)
        .args(["devtools", "--machine", "--no-launch-browser"])
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let pid = child.id()?;
    let stdout = child.stdout.take()?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();

    let result = tokio::time::timeout(Duration::from_secs(6), async {
        while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
            if let Ok(val) = serde_json::from_str::<Value>(line.trim())
                && val.get("event").and_then(Value::as_str) == Some("server.started")
                && let Some(params) = val.get("params")
            {
                let host = params
                    .get("host")
                    .and_then(Value::as_str)
                    .unwrap_or("127.0.0.1")
                    .to_string();
                let port = params.get("port").and_then(Value::as_u64)? as u16;
                return Some(DevToolsServerRecord { pid, host, port });
            }
            line.clear();
        }
        None
    })
    .await
    .ok()
    .flatten();

    if result.is_none() {
        let _ = child.kill().await;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_devtools_url_with_page_from_dds_uri() {
        let url = build_devtools_url(
            Some("http://localhost:9100/?uri=ws%3A%2F%2Flocalhost%3A50000%2Fws"),
            Some("ws://localhost:50000/ws"),
            Some("inspector"),
        )
        .expect("url");
        assert_eq!(
            url,
            "http://localhost:9100/#/inspector?uri=ws%3A%2F%2Flocalhost%3A50000%2Fws"
        );
    }

    #[test]
    fn builds_devtools_url_from_vm_service_uri_fallback() {
        let url = build_devtools_url(
            None,
            Some("ws://localhost:50000/abc/ws"),
            Some("performance"),
        )
        .expect("url");
        assert!(url.starts_with("http://localhost:50000/abc/devtools/#/performance?uri="));
    }

    #[test]
    fn normalizes_page_aliases() {
        assert_eq!(normalize_devtools_page(Some("perf")), Some("performance"));
        assert_eq!(normalize_devtools_page(Some("mem")), Some("memory"));
        assert_eq!(normalize_devtools_page(Some("cpu")), Some("cpu-profiler"));
        assert_eq!(normalize_devtools_page(None), None);
    }
}
