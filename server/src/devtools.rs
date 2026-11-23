use std::fs::{self, File};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::TcpStream;

use crate::cache::DiskCache;
use crate::ipc::is_pid_alive;
use crate::process_util::build_command;
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

/// Builds a direct DevTools URL targeting an optional page without duplicating `uri=`
/// or corrupting DDS paths.
pub fn build_devtools_url(
    devtools_uri: Option<&str>,
    vm_service_uri: Option<&str>,
    page: Option<&str>,
) -> Option<String> {
    let route = normalize_devtools_page(page);

    if let Some(dt) = devtools_uri.map(str::trim).filter(|s| !s.is_empty()) {
        // Parse cleanly using `url::Url` so path, query, and fragment stay intact
        if let Ok(parsed) = url::Url::parse(dt) {
            let origin_and_path = format!(
                "{}://{}{}{}",
                parsed.scheme(),
                parsed.host_str().unwrap_or("localhost"),
                parsed
                    .port()
                    .map(|p| format!(":{p}"))
                    .unwrap_or_default(),
                parsed.path().trim_end_matches('/')
            );

            // Extract existing query either from URL query or from `#/route?uri=...` fragment
            let fragment_query = parsed
                .fragment()
                .and_then(|f| f.split_once('?').map(|(_, q)| q.to_string()));
            let existing_query = parsed
                .query()
                .map(ToOwned::to_owned)
                .or(fragment_query);

            let query = match existing_query {
                Some(q) if !q.is_empty() => q,
                _ => match vm_service_uri {
                    Some(vm) => {
                        let mut ser = url::form_urlencoded::Serializer::new(String::new());
                        ser.append_pair("uri", vm);
                        ser.finish()
                    }
                    None => String::new(),
                },
            };

            return Some(match (route, query.is_empty()) {
                (Some(r), false) => format!("{origin_and_path}/#/{r}?{query}"),
                (Some(r), true) => format!("{origin_and_path}/#/{r}"),
                (None, false) => format!("{origin_and_path}/?{query}"),
                (None, true) => format!("{origin_and_path}/"),
            });
        }
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

/// Ensures a singleton `dart devtools --machine --no-launch-browser` server is running
/// when DDS did not provide a `devtoolsUri`, using file-backed output so `dart devtools`
/// never dies with SIGPIPE when a short-lived CLI process exits.
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
        && probe_tcp(&rec.host, rec.port).await
    {
        return build_devtools_url(Some(&rec.base_url()), Some(vm), page);
    }

    if let Some(rec) = spawn_devtools_server(cache, project_root, custom_sdk_path).await {
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

async fn probe_tcp(host: &str, port: u16) -> bool {
    let addr = format!("{host}:{port}");
    matches!(
        tokio::time::timeout(Duration::from_millis(150), TcpStream::connect(&addr)).await,
        Ok(Ok(_))
    )
}

async fn spawn_devtools_server(
    cache: &DiskCache,
    project_root: &Path,
    custom_sdk_path: Option<&str>,
) -> Option<DevToolsServerRecord> {
    let _ = fs::create_dir_all(cache.root_dir());
    let log_path = cache.root_dir().join("devtools-startup.log");
    let log_file = File::create(&log_path).ok()?;
    let log_err = log_file.try_clone().ok()?;

    let dart_exe = resolve_sdk_executable(project_root, ProjectKind::Dart, custom_sdk_path);
    let mut cmd = build_command(
        &dart_exe,
        ["devtools", "--machine", "--no-launch-browser"],
        project_root,
    );
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_err));

    // Detach into its own process group/session on Unix so parent CLI exit never sends SIGHUP/SIGINT
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.as_std_mut().pre_exec(|| {
                unsafe extern "C" {
                    fn setsid() -> i32;
                }
                let _ = setsid();
                Ok(())
            });
        }
    }

    let mut child = cmd.spawn().ok()?;
    let pid = child.id()?;

    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        if let Ok(text) = fs::read_to_string(&log_path) {
            for line in text.lines() {
                if let Ok(val) = serde_json::from_str::<Value>(line.trim())
                    && val.get("event").and_then(Value::as_str) == Some("server.started")
                    && let Some(params) = val.get("params")
                    && let Some(port) = params.get("port").and_then(Value::as_u64)
                {
                    let host = params
                        .get("host")
                        .and_then(Value::as_str)
                        .unwrap_or("127.0.0.1")
                        .to_string();
                    return Some(DevToolsServerRecord {
                        pid,
                        host,
                        port: port as u16,
                    });
                }
            }
        }
        if let Ok(Some(_)) = child.try_wait() {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(80)).await;
    }

    let _ = child.kill().await;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_devtools_url_with_page_from_dds_uri_without_duplicating_query() {
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

        let from_frag = build_devtools_url(
            Some("http://localhost:9100/#/logging?uri=ws%3A%2F%2Flocalhost%3A50000%2Fws"),
            Some("ws://localhost:50000/ws"),
            Some("performance"),
        )
        .expect("from_frag");
        assert_eq!(
            from_frag,
            "http://localhost:9100/#/performance?uri=ws%3A%2F%2Flocalhost%3A50000%2Fws"
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
}
