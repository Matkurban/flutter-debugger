use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc};

use crate::cache::{DiskCache, FlutterDevice, normalize_project_key, now_unix};
use crate::dap::open_url_in_browser;
use crate::debug_config::run_init_zed;
use crate::devtools::resolve_or_start_devtools_url;
use crate::emulators::{launch_emulator, list_emulators};
use crate::ipc::{
    ActiveSessionRecord, IpcRequest, IpcResponse, OverlayState, ServiceOverlay, find_latest_session,
    generate_session_id, list_active_sessions, remove_session_record_by_id, send_ipc_to_session,
    write_session_record,
};
use crate::project::{ProjectInfo, ProjectKind, find_project_for_path, resolve_sdk_executable};
use crate::runner::strip_ansi_codes;
use crate::vm_service::VmServiceClient;

/// Entrypoint for `flutter-debugger-server ctl <subcommand> [args...]`.
pub async fn run_ctl(args: Vec<String>) -> Result<(), String> {
    let Some(subcmd) = args.first().map(String::as_str) else {
        print_ctl_usage();
        return Ok(());
    };
    let rest = &args[1..];

    match subcmd {
        "hot-reload" | "reload" | "r" => {
            let project = parse_project_flag(rest);
            send_simple_session_cmd(project.as_deref(), IpcRequest::HotReload).await
        }
        "hot-restart" | "restart" | "R" => {
            let project = parse_project_flag(rest);
            send_simple_session_cmd(project.as_deref(), IpcRequest::HotRestart).await
        }
        "stop" | "q" => {
            let project = parse_project_flag(rest);
            send_simple_session_cmd(project.as_deref(), IpcRequest::Stop).await
        }
        "status" | "sessions" => cmd_status(rest).await,
        "devtools" | "dt" => cmd_devtools(rest).await,
        "overlay" => cmd_overlay(rest).await,
        "screenshot" => cmd_screenshot(rest).await,
        "devices" => cmd_devices(rest).await,
        "select-device" | "device" => cmd_select_device(rest).await,
        "emulators" => cmd_emulators(rest).await,
        "launch-emulator" | "emulator" => cmd_launch_emulator(rest).await,
        "run" => cmd_run(rest).await,
        "build-runner" | "br" => cmd_build_runner(rest).await,
        "pub-get" => cmd_passthrough_flutter(rest, &["pub", "get"]).await,
        "clean" => cmd_passthrough_flutter(rest, &["clean"]).await,
        "doctor" => cmd_passthrough_flutter(rest, &["doctor", "-v"]).await,
        "create" => cmd_create_interactive(rest).await,
        "init-zed" => run_init_zed(rest),
        "print-zed-config" => {
            print_recommended_zed_config();
            Ok(())
        }
        "--help" | "-h" | "help" => {
            print_ctl_usage();
            Ok(())
        }
        other => Err(format!(
            "Unknown ctl subcommand `{other}`. Run `flutter-debugger-server ctl --help`."
        )),
    }
}

fn print_ctl_usage() {
    println!(
        "flutter-debugger-server ctl <subcommand> [options]

Session Control:
  hot-reload [--project <dir>]           Trigger Hot Reload on active session
  hot-restart [--project <dir>]          Trigger Hot Restart on active session
  stop [--project <dir>]                 Stop active Flutter session
  status [--project <dir>]               Show active sessions
  devtools [--page <name>]               Open DevTools (inspector|performance|memory|network|logging|cpu-profiler)
  overlay <name> [--project <dir>]       Toggle overlay (inspector|performance|debug-paint|baselines|repaint|slow-anim|banner|oversized|platform|brightness)
  screenshot [--out <path>]              Capture screenshot from active session or device

Devices & Emulators:
  devices                                List connected Flutter devices
  select-device [device_id]              Select & persist target device (interactive if omitted)
  emulators                              List available emulators
  launch-emulator [emulator_id]          Launch emulator (interactive if omitted)

Run & Workflow:
  run [--mode debug|profile|release] [--target lib/main.dart] [--device <id>] [--flavor <f>] [--dart-define-from-file <file>]
  build-runner <build|watch>             Run dart build_runner with --delete-conflicting-outputs
  pub-get | clean | doctor               Run standard Flutter/Dart CLI commands
  create                                 Interactive `flutter create` prompt
  print-zed-config                       Print recommended Zed settings.json & keymap.json"
    );
}

fn resolve_project_root(flag_dir: Option<&Path>) -> PathBuf {
    let base = flag_dir
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    find_project_for_path(&base)
        .map(|p| p.root)
        .unwrap_or(base)
}

fn resolve_project_info(flag_dir: Option<&Path>) -> Result<ProjectInfo, String> {
    let base = flag_dir
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    find_project_for_path(&base)
        .ok_or_else(|| format!("No pubspec.yaml found in or above `{}`", base.display()))
}

fn parse_project_flag(args: &[String]) -> Option<PathBuf> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--project" || arg == "-p" {
            return iter.next().map(PathBuf::from);
        }
        if let Some(val) = arg.strip_prefix("--project=") {
            return Some(PathBuf::from(val));
        }
    }
    None
}

fn parse_flag_value(args: &[String], long_name: &str, short_name: Option<&str>) -> Option<String> {
    let prefix = format!("{long_name}=");
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == long_name || short_name.is_some_and(|s| arg == s) {
            return iter.next().cloned();
        }
        if let Some(val) = arg.strip_prefix(&prefix) {
            return Some(val.to_string());
        }
    }
    None
}

async fn send_simple_session_cmd(
    project_flag: Option<&Path>,
    request: IpcRequest,
) -> Result<(), String> {
    let cache = DiskCache::default_location();
    let root = resolve_project_root(project_flag);
    let session = find_latest_session(&cache, Some(&root))
        .or_else(|| find_latest_session(&cache, None))
        .ok_or_else(|| {
            format!(
                "No active Flutter session found for `{}`. Start with F4 (Debug) or `ctl run`.",
                root.display()
            )
        })?;

    match send_ipc_to_session(&cache, &session, &request).await {
        Some(resp) if resp.ok => {
            println!("{}", resp.message);
            Ok(())
        }
        Some(resp) => Err(resp.message),
        None => Err("Active Flutter session did not respond (it may have exited).".to_string()),
    }
}

async fn cmd_status(args: &[String]) -> Result<(), String> {
    let cache = DiskCache::default_location();
    let project_flag = parse_project_flag(args);
    let sessions = if let Some(p) = project_flag {
        let root = resolve_project_root(Some(&p));
        crate::ipc::find_sessions_for_project(&cache, &root)
    } else {
        list_active_sessions(&cache)
    };

    if sessions.is_empty() {
        println!("No active Flutter sessions.");
        return Ok(());
    }

    for s in sessions {
        println!(
            "[{}] {} | mode={} | device={} | pid={} | vm={}",
            s.kind,
            s.project_root,
            s.mode,
            s.device_id.as_deref().unwrap_or("auto"),
            s.pid,
            s.vm_service_uri.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

async fn cmd_devtools(args: &[String]) -> Result<(), String> {
    let project_flag = parse_project_flag(args);
    let page = parse_flag_value(args, "--page", None);
    let cache = DiskCache::default_location();
    let root = resolve_project_root(project_flag.as_deref());
    let session = find_latest_session(&cache, Some(&root))
        .or_else(|| find_latest_session(&cache, None))
        .ok_or_else(|| "No active Flutter session found.".to_string())?;

    match send_ipc_to_session(
        &cache,
        &session,
        &IpcRequest::OpenDevTools { page: page.clone() },
    )
    .await
    {
        Some(resp) if resp.ok => {
            println!("{}", resp.message);
            Ok(())
        }
        Some(resp) => Err(resp.message),
        None => {
            let custom_sdk = std::env::var("FLUTTER_SDK_PATH").ok();
            if let Some(url) = resolve_or_start_devtools_url(
                &cache,
                &root,
                session.devtools_uri.as_deref(),
                session.vm_service_uri.as_deref(),
                page.as_deref(),
                custom_sdk.as_deref(),
            )
            .await
            {
                open_url_in_browser(&url);
                println!("Opened DevTools: {url}");
                Ok(())
            } else {
                Err("DevTools URI is not available for this session.".to_string())
            }
        }
    }
}

async fn cmd_overlay(args: &[String]) -> Result<(), String> {
    let project_flag = parse_project_flag(args);
    let name = args.iter().find(|a| !a.starts_with('-')).ok_or_else(|| {
        "Missing overlay name. Choices: inspector, performance, debug-paint, baselines, repaint, slow-anim, banner, oversized, platform, brightness".to_string()
    })?;

    let overlay = ServiceOverlay::parse(name).ok_or_else(|| {
        format!(
            "Unknown overlay `{name}`. Choices: inspector, performance, debug-paint, baselines, repaint, slow-anim, banner, oversized, platform, brightness"
        )
    })?;

    send_simple_session_cmd(project_flag.as_deref(), IpcRequest::ToggleOverlay { overlay }).await
}

async fn cmd_screenshot(args: &[String]) -> Result<(), String> {
    let project_flag = parse_project_flag(args);
    let out_flag = parse_flag_value(args, "--out", Some("-o"));
    let root = resolve_project_root(project_flag.as_deref());

    let output_path = out_flag.map(|o| {
        let p = PathBuf::from(&o);
        if p.is_dir() {
            p.join(format!("flutter_screenshot_{}.png", now_unix()))
                .to_string_lossy()
                .into_owned()
        } else {
            o
        }
    });

    send_simple_session_cmd(Some(&root), IpcRequest::Screenshot { output_path }).await
}

async fn fetch_devices_live(root: &Path) -> Result<Vec<FlutterDevice>, String> {
    let custom_sdk = std::env::var("FLUTTER_SDK_PATH").ok();
    let exe = resolve_sdk_executable(root, ProjectKind::Flutter, custom_sdk.as_deref());
    let out = spawn_output(&exe, &["devices", "--machine"], root)
        .await
        .map_err(|e| format!("Failed to run `{exe} devices --machine`: {e}"))?;
    if !out.status.success() {
        return Err(strip_ansi_codes(&String::from_utf8_lossy(&out.stderr)));
    }
    let stdout = strip_ansi_codes(&String::from_utf8_lossy(&out.stdout));
    let devices: Vec<FlutterDevice> =
        serde_json::from_str(&stdout).map_err(|e| format!("Failed to parse device list: {e}"))?;
    DiskCache::default_location().save_devices(&devices);
    Ok(devices)
}

async fn cmd_devices(args: &[String]) -> Result<(), String> {
    let root = resolve_project_root(parse_project_flag(args).as_deref());
    let cache = DiskCache::default_location();
    let selected = cache.get_selected_device(&root);
    let devices = fetch_devices_live(&root).await?;

    if devices.is_empty() {
        println!("No connected Flutter devices found.");
        return Ok(());
    }

    for (idx, dev) in devices.iter().enumerate() {
        let marker = if selected.as_deref() == Some(dev.id.as_str()) {
            "*"
        } else {
            " "
        };
        println!(
            "{marker} [{}] {} ({}) - {}",
            idx + 1,
            dev.name,
            dev.id,
            dev.target_platform
        );
    }
    Ok(())
}

async fn cmd_select_device(args: &[String]) -> Result<(), String> {
    let project_flag = parse_project_flag(args);
    let root = resolve_project_root(project_flag.as_deref());
    let cache = DiskCache::default_location();

    let positional = first_positional_arg(args);
    if let Some(id) = positional {
        cache.set_selected_device(&root, id);
        println!("Selected Flutter device `{id}` for {}", root.display());
        return Ok(());
    }

    let devices = fetch_devices_live(&root).await?;
    if devices.is_empty() {
        return Err("No Flutter devices found. Connect a device or launch an emulator.".to_string());
    }

    let current = cache.get_selected_device(&root);
    println!("Select target Flutter device for {}:", root.display());
    for (idx, dev) in devices.iter().enumerate() {
        let marker = if current.as_deref() == Some(dev.id.as_str()) {
            " (current)"
        } else {
            ""
        };
        println!("  {}) {} [{}]{marker}", idx + 1, dev.display_label(), dev.id);
    }

    let chosen_idx = prompt_number("Enter number (or q to cancel): ", devices.len())?;
    let dev = &devices[chosen_idx];
    cache.set_selected_device(&root, &dev.id);
    println!("Selected device: {} ({})", dev.display_label(), dev.id);
    Ok(())
}

async fn cmd_emulators(args: &[String]) -> Result<(), String> {
    let root = resolve_project_root(parse_project_flag(args).as_deref());
    let custom_sdk = std::env::var("FLUTTER_SDK_PATH").ok();
    let emulators = list_emulators(&root, custom_sdk.as_deref()).await?;

    if emulators.is_empty() {
        println!("No Flutter emulators found.");
        return Ok(());
    }
    for (idx, emu) in emulators.iter().enumerate() {
        println!("  {}) {} ({})", idx + 1, emu.display_label(), emu.id);
    }
    Ok(())
}

async fn cmd_launch_emulator(args: &[String]) -> Result<(), String> {
    let project_flag = parse_project_flag(args);
    let root = resolve_project_root(project_flag.as_deref());
    let custom_sdk = std::env::var("FLUTTER_SDK_PATH").ok();

    if let Some(id) = first_positional_arg(args) {
        println!("Launching emulator `{id}`...");
        launch_emulator(&root, id, custom_sdk.as_deref()).await?;
        println!("Launched emulator `{id}`.");
        return Ok(());
    }

    let emulators = list_emulators(&root, custom_sdk.as_deref()).await?;
    if emulators.is_empty() {
        return Err("No Flutter emulators found. Create an AVD or install Xcode Simulator.".into());
    }

    println!("Available emulators:");
    for (idx, emu) in emulators.iter().enumerate() {
        println!("  {}) {} ({})", idx + 1, emu.display_label(), emu.id);
    }

    let chosen_idx = prompt_number("Enter number to launch (or q to cancel): ", emulators.len())?;
    let emu = &emulators[chosen_idx];
    println!("Launching {}...", emu.display_label());
    launch_emulator(&root, &emu.id, custom_sdk.as_deref()).await?;
    println!("Launched emulator: {}", emu.display_label());
    Ok(())
}

/// Native terminal `flutter run` wrapper that preserves full ANSI color output while
/// registering into the unified session center so `did_save` Hot Reload and `ctl` commands work.
async fn cmd_run(args: &[String]) -> Result<(), String> {
    let project_flag = parse_project_flag(args);
    let project = resolve_project_info(project_flag.as_deref())?;
    if project.kind != ProjectKind::Flutter {
        return Err("`ctl run` is only supported for Flutter projects.".to_string());
    }

    let mode = parse_flag_value(args, "--mode", Some("-m")).unwrap_or_else(|| "debug".to_string());
    let target =
        parse_flag_value(args, "--target", Some("-t")).unwrap_or_else(|| "lib/main.dart".into());
    let flavor = parse_flag_value(args, "--flavor", None);
    let dart_define_from_file = parse_flag_value(args, "--dart-define-from-file", None);
    let cache = DiskCache::default_location();
    let device_id = parse_flag_value(args, "--device", Some("-d"))
        .or_else(|| cache.get_selected_device(&project.root));

    let extra_args: Vec<String> = args
        .iter()
        .position(|a| a == "--")
        .map(|idx| args[idx + 1..].to_vec())
        .unwrap_or_default();

    let mode_flag = match mode.to_ascii_lowercase().as_str() {
        "profile" => "--profile",
        "release" => "--release",
        _ => "--debug",
    };

    let mut flutter_args = vec![
        "run".to_string(),
        mode_flag.to_string(),
        "-t".to_string(),
        target,
    ];
    if let Some(ref dev) = device_id
        && !dev.trim().is_empty()
    {
        flutter_args.push("-d".to_string());
        flutter_args.push(dev.clone());
    }
    if let Some(ref flav) = flavor
        && !flav.trim().is_empty()
    {
        flutter_args.push("--flavor".to_string());
        flutter_args.push(flav.clone());
    }
    if let Some(ref define_file) = dart_define_from_file
        && !define_file.trim().is_empty()
    {
        flutter_args.push(format!("--dart-define-from-file={define_file}"));
    }
    flutter_args.extend(extra_args);

    let custom_sdk = std::env::var("FLUTTER_SDK_PATH").ok();
    let exe = resolve_sdk_executable(&project.root, ProjectKind::Flutter, custom_sdk.as_deref());

    let mut child = spawn_interactive(&exe, &flutter_args, &project.root)
        .map_err(|e| format!("Failed to spawn `{exe}`: {e}"))?;

    let child_stdin = child.stdin.take().ok_or("Failed to capture child stdin")?;
    let child_stdout = child.stdout.take().ok_or("Failed to capture child stdout")?;
    let child_stderr = child.stderr.take().ok_or("Failed to capture child stderr")?;

    let ipc_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to bind local IPC port: {e}"))?;
    let ipc_port = ipc_listener.local_addr().map_err(|e| e.to_string())?.port();

    let session_id = generate_session_id();
    let run_state = Arc::new(Mutex::new(RunSessionState {
        session_id: session_id.clone(),
        project_root: project.root.clone(),
        device_id: device_id.clone(),
        mode: mode.to_ascii_lowercase(),
        vm_service_uri: None,
        devtools_uri: None,
        overlays: OverlayState::default(),
    }));

    persist_run_session(&cache, ipc_port, &run_state).await;
    let _cleanup = RunSessionCleanupGuard {
        cache: cache.clone(),
        session_id,
    };

    let (stdin_tx, mut stdin_rx) = mpsc::channel::<Vec<u8>>(64);
    let stdin_writer = tokio::spawn(async move {
        let mut writer = child_stdin;
        while let Some(bytes) = stdin_rx.recv().await {
            if writer.write_all(&bytes).await.is_err() {
                break;
            }
            let _ = writer.flush().await;
        }
    });

    let term_tx = stdin_tx.clone();
    let term_reader = tokio::spawn(async move {
        let mut stdin = BufReader::new(tokio::io::stdin());
        let mut line = String::new();
        while stdin.read_line(&mut line).await.unwrap_or(0) > 0 {
            if term_tx.send(line.as_bytes().to_vec()).await.is_err() {
                break;
            }
            line.clear();
        }
    });

    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(child_stderr);
        let mut buf = String::new();
        while reader.read_line(&mut buf).await.unwrap_or(0) > 0 {
            eprint!("{buf}");
            let _ = io::stderr().flush();
            buf.clear();
        }
    });

    let out_state = Arc::clone(&run_state);
    let out_cache = cache.clone();
    let stdout_task = tokio::spawn(async move {
        let mut reader = BufReader::new(child_stdout);
        let mut buf = String::new();
        while reader.read_line(&mut buf).await.unwrap_or(0) > 0 {
            print!("{buf}");
            let _ = io::stdout().flush();

            let cleaned = strip_ansi_codes(&buf);
            let mut updated = false;
            {
                let mut guard = out_state.lock().await;
                if let Some(vm) = extract_vm_service_uri(&cleaned)
                    && guard.vm_service_uri.as_deref() != Some(vm.as_str())
                {
                    guard.vm_service_uri = Some(vm);
                    updated = true;
                }
                if let Some(dt) = extract_devtools_uri(&cleaned)
                    && guard.devtools_uri.as_deref() != Some(dt.as_str())
                {
                    guard.devtools_uri = Some(dt);
                    updated = true;
                }
            }
            if updated {
                persist_run_session(&out_cache, ipc_port, &out_state).await;
            }
            buf.clear();
        }
    });

    let ipc_state = Arc::clone(&run_state);
    let ipc_stdin = stdin_tx.clone();
    let ipc_cache = cache.clone();
    let ipc_sdk = custom_sdk.clone();
    let ipc_task = tokio::spawn(async move {
        while let Ok((stream, _)) = ipc_listener.accept().await {
            let st = Arc::clone(&ipc_state);
            let tx = ipc_stdin.clone();
            let c = ipc_cache.clone();
            let sdk = ipc_sdk.clone();
            tokio::spawn(async move {
                handle_run_ipc(stream, st, tx, c, sdk).await;
            });
        }
    });

    let status = child.wait().await.map_err(|e| e.to_string())?;
    ipc_task.abort();
    term_reader.abort();
    drop(stdin_tx);
    let _ = stdin_writer.await;
    let _ = stdout_task.await;
    let _ = stderr_task.await;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "flutter run exited with status {}",
            status.code().unwrap_or(-1)
        ))
    }
}

struct RunSessionState {
    session_id: String,
    project_root: PathBuf,
    device_id: Option<String>,
    mode: String,
    vm_service_uri: Option<String>,
    devtools_uri: Option<String>,
    overlays: OverlayState,
}

async fn persist_run_session(cache: &DiskCache, port: u16, state: &Arc<Mutex<RunSessionState>>) {
    let guard = state.lock().await;
    let record = ActiveSessionRecord {
        session_id: guard.session_id.clone(),
        project_root: normalize_project_key(&guard.project_root),
        device_id: guard.device_id.clone(),
        mode: guard.mode.clone(),
        pid: std::process::id(),
        ipc_port: port,
        vm_service_uri: guard.vm_service_uri.clone(),
        devtools_uri: guard.devtools_uri.clone(),
        started_at: now_unix(),
        is_test: false,
        kind: "run".to_string(),
    };
    let _ = write_session_record(cache, &record);
}

struct RunSessionCleanupGuard {
    cache: DiskCache,
    session_id: String,
}

impl Drop for RunSessionCleanupGuard {
    fn drop(&mut self) {
        remove_session_record_by_id(&self.cache, &self.session_id);
    }
}

async fn handle_run_ipc(
    mut stream: tokio::net::TcpStream,
    state: Arc<Mutex<RunSessionState>>,
    stdin_tx: mpsc::Sender<Vec<u8>>,
    cache: DiskCache,
    custom_sdk: Option<String>,
) {
    let (read_half, mut write_half) = stream.split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }

    let response = match serde_json::from_str::<IpcRequest>(line.trim()) {
        Ok(IpcRequest::HotReload) => {
            let (mode, dev, vm, dt) = {
                let g = state.lock().await;
                (
                    g.mode.clone(),
                    g.device_id.clone(),
                    g.vm_service_uri.clone(),
                    g.devtools_uri.clone(),
                )
            };
            if mode != "debug" {
                IpcResponse::err(format!("Hot Reload is disabled in `{mode}` mode"))
            } else {
                let start = Instant::now();
                let sent = stdin_tx.send(b"r".to_vec()).await.is_ok();
                let ms = start.elapsed().as_millis() as u64;
                IpcResponse {
                    ok: sent,
                    message: if sent {
                        format!("Hot Reload triggered ({ms}ms)")
                    } else {
                        "Flutter run session closed".to_string()
                    },
                    duration_ms: Some(ms),
                    mode: Some(mode),
                    device_id: dev,
                    vm_service_uri: vm,
                    devtools_uri: dt,
                }
            }
        }
        Ok(IpcRequest::HotRestart) => {
            let (mode, dev, vm, dt) = {
                let g = state.lock().await;
                (
                    g.mode.clone(),
                    g.device_id.clone(),
                    g.vm_service_uri.clone(),
                    g.devtools_uri.clone(),
                )
            };
            if mode != "debug" {
                IpcResponse::err(format!("Hot Restart is disabled in `{mode}` mode"))
            } else {
                let sent = stdin_tx.send(b"R".to_vec()).await.is_ok();
                IpcResponse {
                    ok: sent,
                    message: if sent {
                        "Hot Restart triggered".to_string()
                    } else {
                        "Flutter run session closed".to_string()
                    },
                    duration_ms: None,
                    mode: Some(mode),
                    device_id: dev,
                    vm_service_uri: vm,
                    devtools_uri: dt,
                }
            }
        }
        Ok(IpcRequest::Stop) => {
            let _ = stdin_tx.send(b"q".to_vec()).await;
            IpcResponse {
                ok: true,
                message: "Stopping Flutter application...".to_string(),
                duration_ms: None,
                mode: None,
                device_id: None,
                vm_service_uri: None,
                devtools_uri: None,
            }
        }
        Ok(IpcRequest::ToggleOverlay { overlay }) => {
            let (mode, dev, vm, dt, method, params, label) = {
                let mut g = state.lock().await;
                if g.mode == "release" {
                    write_ipc_resp(
                        &mut write_half,
                        &IpcResponse::err("Visual overlays are unavailable in release mode"),
                    )
                    .await;
                    return;
                }
                let (m, p, l) = g.overlays.toggle(overlay);
                (
                    g.mode.clone(),
                    g.device_id.clone(),
                    g.vm_service_uri.clone(),
                    g.devtools_uri.clone(),
                    m,
                    p,
                    l,
                )
            };
            if let Some(vm_uri) = vm.as_deref() {
                match VmServiceClient::connect(vm_uri).await {
                    Ok(mut client) => match client.call_service_extension(method, params).await {
                        Ok(_) => IpcResponse {
                            ok: true,
                            message: label,
                            duration_ms: None,
                            mode: Some(mode),
                            device_id: dev,
                            vm_service_uri: vm,
                            devtools_uri: dt,
                        },
                        Err(e) => {
                            IpcResponse::err(format!("Failed to toggle {}: {e}", overlay.label()))
                        }
                    },
                    Err(e) => IpcResponse::err(e),
                }
            } else {
                IpcResponse::err("VM Service URI not ready yet")
            }
        }
        Ok(IpcRequest::CallServiceExtension { method, params }) => {
            let vm = state.lock().await.vm_service_uri.clone();
            if let Some(vm_uri) = vm.as_deref() {
                match VmServiceClient::connect(vm_uri).await {
                    Ok(mut client) => match client.call_service_extension(&method, params).await {
                        Ok(_) => IpcResponse {
                            ok: true,
                            message: format!("Called `{method}`"),
                            duration_ms: None,
                            mode: None,
                            device_id: None,
                            vm_service_uri: vm,
                            devtools_uri: None,
                        },
                        Err(e) => IpcResponse::err(e),
                    },
                    Err(e) => IpcResponse::err(e),
                }
            } else {
                IpcResponse::err("VM Service URI not ready yet")
            }
        }
        Ok(IpcRequest::OpenDevTools { page }) => {
            let (root, dt, vm) = {
                let g = state.lock().await;
                (
                    g.project_root.clone(),
                    g.devtools_uri.clone(),
                    g.vm_service_uri.clone(),
                )
            };
            if let Some(url) = resolve_or_start_devtools_url(
                &cache,
                &root,
                dt.as_deref(),
                vm.as_deref(),
                page.as_deref(),
                custom_sdk.as_deref(),
            )
            .await
            {
                open_url_in_browser(&url);
                IpcResponse {
                    ok: true,
                    message: format!("Opened DevTools: {url}"),
                    duration_ms: None,
                    mode: None,
                    device_id: None,
                    vm_service_uri: vm,
                    devtools_uri: Some(url),
                }
            } else {
                IpcResponse::err("DevTools URI is not ready yet")
            }
        }
        Ok(IpcRequest::Screenshot { output_path }) => {
            let (root, vm) = {
                let g = state.lock().await;
                (g.project_root.clone(), g.vm_service_uri.clone())
            };
            let out_file = output_path.unwrap_or_else(|| {
                root.join(format!("flutter_screenshot_{}.png", now_unix()))
                    .to_string_lossy()
                    .into_owned()
            });
            if let Some(vm_uri) = vm.as_deref()
                && let Ok(mut client) = VmServiceClient::connect(vm_uri).await
                && let Ok(png) = client.capture_screenshot_png().await
                && fs::write(&out_file, png).is_ok()
            {
                IpcResponse {
                    ok: true,
                    message: format!("Screenshot saved to {out_file}"),
                    duration_ms: None,
                    mode: None,
                    device_id: None,
                    vm_service_uri: vm,
                    devtools_uri: None,
                }
            } else {
                IpcResponse::err("Could not capture screenshot via VM Service")
            }
        }
        Ok(IpcRequest::Status) => {
            let g = state.lock().await;
            IpcResponse {
                ok: true,
                message: "active".to_string(),
                duration_ms: None,
                mode: Some(g.mode.clone()),
                device_id: g.device_id.clone(),
                vm_service_uri: g.vm_service_uri.clone(),
                devtools_uri: g.devtools_uri.clone(),
            }
        }
        Err(e) => IpcResponse::err(format!("Invalid IPC request: {e}")),
    };

    write_ipc_resp(&mut write_half, &response).await;
}

async fn write_ipc_resp(write_half: &mut tokio::net::tcp::WriteHalf<'_>, resp: &IpcResponse) {
    if let Ok(mut payload) = serde_json::to_string(resp) {
        payload.push('\n');
        let _ = write_half.write_all(payload.as_bytes()).await;
    }
}

async fn cmd_build_runner(args: &[String]) -> Result<(), String> {
    let project_flag = parse_project_flag(args);
    let project = resolve_project_info(project_flag.as_deref())?;
    let pubspec_text = fs::read_to_string(&project.pubspec_path).unwrap_or_default();
    if !pubspec_text.contains("build_runner") {
        return Err("`build_runner` is not listed in pubspec.yaml.".to_string());
    }

    let action = first_positional_arg(args).unwrap_or("build");
    if action != "build" && action != "watch" {
        return Err(format!(
            "Unsupported build-runner action `{action}` (expected `build` or `watch`)."
        ));
    }

    let custom_sdk = std::env::var("FLUTTER_SDK_PATH").ok();
    let dart_exe = resolve_sdk_executable(&project.root, ProjectKind::Dart, custom_sdk.as_deref());
    let status = Command::new(&dart_exe)
        .args(["run", "build_runner", action, "--delete-conflicting-outputs"])
        .current_dir(&project.root)
        .status()
        .await
        .map_err(|e| format!("Failed to run build_runner: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "build_runner exited with code {}",
            status.code().unwrap_or(-1)
        ))
    }
}

async fn cmd_passthrough_flutter(args: &[String], subargs: &[&str]) -> Result<(), String> {
    let project_flag = parse_project_flag(args);
    let root = resolve_project_root(project_flag.as_deref());
    let kind = find_project_for_path(&root)
        .map(|p| p.kind)
        .unwrap_or(ProjectKind::Flutter);
    let custom_sdk = std::env::var("FLUTTER_SDK_PATH").ok();
    let exe = resolve_sdk_executable(&root, kind, custom_sdk.as_deref());

    let status = Command::new(&exe)
        .args(subargs)
        .current_dir(&root)
        .status()
        .await
        .map_err(|e| format!("Failed to execute `{exe}`: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "`{exe} {}` exited with code {}",
            subargs.join(" "),
            status.code().unwrap_or(-1)
        ))
    }
}

async fn cmd_create_interactive(args: &[String]) -> Result<(), String> {
    let base_dir = parse_project_flag(args)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let name = prompt_string(
        "Project name (snake_case) [my_flutter_app]: ",
        "my_flutter_app",
    )?;
    let org = prompt_string("Organization [com.example]: ", "com.example")?;
    let template = prompt_string(
        "Template (app|package|plugin|plugin_ffi|module) [app]: ",
        "app",
    )?;

    let target = base_dir.join(&name);
    let custom_sdk = std::env::var("FLUTTER_SDK_PATH").ok();
    let exe = resolve_sdk_executable(&base_dir, ProjectKind::Flutter, custom_sdk.as_deref());

    let status = Command::new(&exe)
        .args([
            "create",
            "--project-name",
            &name,
            "--org",
            &org,
            &format!("--template={template}"),
            &target.to_string_lossy(),
        ])
        .current_dir(&base_dir)
        .status()
        .await
        .map_err(|e| format!("Failed to run flutter create: {e}"))?;

    if status.success() {
        println!("Created Flutter project at {}", target.display());
        Ok(())
    } else {
        Err("flutter create failed".to_string())
    }
}

pub fn extract_vm_service_uri(line: &str) -> Option<String> {
    if !line.contains("http://") && !line.contains("ws://") {
        return None;
    }
    for token in line.split_whitespace() {
        let cleaned = token.trim_matches(|c: char| matches!(c, '(' | ')' | ',' | '.' | '"' | '\''));
        if let Ok(parsed) = url::Url::parse(cleaned)
            && matches!(parsed.scheme(), "http" | "ws")
            && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost"))
            && !cleaned.contains("devtools")
            && parsed.port().is_some()
        {
            return Some(cleaned.to_string());
        }
    }
    None
}

pub fn extract_devtools_uri(line: &str) -> Option<String> {
    if !line.contains("devtools") && !line.contains("?uri=") {
        return None;
    }
    for token in line.split_whitespace() {
        let cleaned = token.trim_matches(|c: char| matches!(c, '(' | ')' | ',' | '.' | '"' | '\''));
        if let Ok(parsed) = url::Url::parse(cleaned)
            && matches!(parsed.scheme(), "http" | "https")
            && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost"))
            && (cleaned.contains("devtools") || parsed.query().is_some())
        {
            return Some(cleaned.to_string());
        }
    }
    None
}

fn first_positional_arg(args: &[String]) -> Option<&str> {
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if matches!(arg.as_str(), "--project" | "-p" | "--out" | "-o" | "--page") {
            skip_next = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg.as_str());
    }
    None
}

fn prompt_number(prompt: &str, max: usize) -> Result<usize, String> {
    print!("{prompt}");
    let _ = io::stdout().flush();
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| format!("Failed to read input: {e}"))?;
    let trimmed = input.trim();
    if trimmed.eq_ignore_ascii_case("q") || trimmed.is_empty() {
        return Err("Cancelled.".to_string());
    }
    let num: usize = trimmed
        .parse()
        .map_err(|_| format!("Invalid selection `{trimmed}`"))?;
    if num == 0 || num > max {
        return Err(format!("Selection out of range (1..={max})"));
    }
    Ok(num - 1)
}

fn prompt_string(prompt: &str, default: &str) -> Result<String, String> {
    print!("{prompt}");
    let _ = io::stdout().flush();
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| format!("Failed to read input: {e}"))?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

async fn spawn_output(
    exe: &str,
    args: &[&str],
    cwd: &Path,
) -> std::io::Result<std::process::Output> {
    Command::new(exe)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .await
}

fn spawn_interactive(exe: &str, args: &[String], cwd: &Path) -> std::io::Result<Child> {
    Command::new(exe)
        .args(args)
        .current_dir(cwd)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

pub fn print_recommended_zed_config() {
    println!(
        r#"=== Recommended ~/.config/zed/settings.json snippet ===
{{
  "lsp_document_colors": "inlay",
  "inlay_hints": {{
    "enabled": true
  }},
  "lsp": {{
    "flutter-debugger": {{
      "initialization_options": {{
        "autoHotReloadOnSave": true,
        "closingLabels": true,
        "closingLabelsMinLines": 2,
        "colorPreviews": true,
        "enablePubVersionHints": false,
        "exposeGlobalCodeActions": false
      }}
    }}
  }}
}}

=== Recommended ~/.config/zed/keymap.json snippet ===
[
  {{
    "context": "Workspace",
    "bindings": {{
      "cmd-alt-r": ["task::Spawn", {{ "task_name": "Flutter: Hot Reload" }}],
      "cmd-shift-alt-r": ["task::Spawn", {{ "task_name": "Flutter: Hot Restart" }}],
      "cmd-alt-d": ["task::Spawn", {{ "task_name": "Flutter: Select Device" }}],
      "cmd-alt-e": ["task::Spawn", {{ "task_name": "Flutter: Launch Emulator" }}],
      "cmd-alt-i": ["task::Spawn", {{ "task_name": "Flutter: Toggle Widget Inspector" }}],
      "cmd-alt-p": ["task::Spawn", {{ "task_name": "Flutter: Open DevTools (Inspector)" }}],
      "cmd-alt-b": ["task::Spawn", {{ "task_name": "Flutter: Toggle Brightness" }}],
      "cmd-alt-s": ["task::Spawn", {{ "task_name": "Flutter: Stop App" }}]
    }}
  }}
]"#
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_vm_service_and_devtools_uris_from_flutter_run_output() {
        let vm_line = "A Dart VM Service on macOS is available at: http://localhost:54321/session/";
        assert_eq!(
            extract_vm_service_uri(vm_line).as_deref(),
            Some("http://localhost:54321/session/")
        );

        let dt_line = "The Flutter DevTools debugger and profiler on macOS is available at: http://localhost:9100?uri=http://localhost:54321/session/";
        assert_eq!(
            extract_devtools_uri(dt_line).as_deref(),
            Some("http://localhost:9100?uri=http://localhost:54321/session/")
        );
    }

    #[test]
    fn parses_project_and_positional_flags() {
        let args = vec![
            "--project".to_string(),
            "/tmp/my_app".to_string(),
            "macos".to_string(),
        ];
        assert_eq!(parse_project_flag(&args), Some(PathBuf::from("/tmp/my_app")));
        assert_eq!(first_positional_arg(&args), Some("macos"));
    }
}
