use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::cache::{
    DiskCache, FlutterDevice, extract_device_flag_from_args, normalize_project_key, now_unix,
    resolve_effective_device,
};
use crate::dap::open_url_in_browser;
use crate::debug_config::run_init_zed;
use crate::devtools::resolve_or_start_devtools_url;
use crate::emulators::{launch_emulator, list_emulators};
use crate::ipc::{
    ActiveSessionRecord, DebugLibraryScope, ExceptionPauseMode, IpcRequest, IpcResponse,
    OverlayState, ServiceOverlay, find_latest_session, generate_session_id, list_active_sessions,
    remove_session_record_by_id, send_ipc_to_session, write_session_record,
};
use crate::process_util::{build_command, terminate_child_tree};
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
        "devtools-ui" | "dt-ui" | "gpui" => cmd_devtools_ui(rest),
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
  devtools-ui [--project <dir>] [--page <inspector|network|logging|performance|memory>]
                                         Open Native GPUI Flutter DevTools companion app
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

fn resolve_session_for_ctl(
    cache: &DiskCache,
    project_flag: Option<&Path>,
    root: &Path,
) -> Option<ActiveSessionRecord> {
    if project_flag.is_some() {
        find_latest_session(cache, Some(root))
    } else {
        find_latest_session(cache, Some(root)).or_else(|| find_latest_session(cache, None))
    }
}

async fn send_simple_session_cmd(
    project_flag: Option<&Path>,
    request: IpcRequest,
) -> Result<(), String> {
    let cache = DiskCache::default_location();
    let root = resolve_project_root(project_flag);
    let session = resolve_session_for_ctl(&cache, project_flag, &root).ok_or_else(|| {
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
    let session = resolve_session_for_ctl(&cache, project_flag.as_deref(), &root)
        .ok_or_else(|| format!("No active Flutter session found for `{}`." , root.display()))?;

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

fn cmd_devtools_ui(args: &[String]) -> Result<(), String> {
    let project_flag = parse_project_flag(args);
    let page = parse_flag_value(args, "--page", None);
    let root = resolve_project_root(project_flag.as_deref());
    let msg = launch_native_devtools_ui(Some(&root), page.as_deref())?;
    println!("{msg}");
    Ok(())
}

/// Resolves `flutter-devtools-gpui` (sibling binary -> `~/.local/bin/flutter-devtools-gpui` -> `$PATH`)
/// and spawns it in a detached session (`setsid` + `Stdio::null()`).
pub fn launch_native_devtools_ui(
    project_root: Option<&Path>,
    page: Option<&str>,
) -> Result<String, String> {
    let bin = resolve_native_devtools_binary().ok_or_else(|| {
        "Native GPUI DevTools binary `flutter-devtools-gpui` not found.\n\
         Build and install it with:\n\
         ./scripts/install-local.sh\n\
         (or `cd devtools-gpui && cargo build --release && cp target/release/flutter-devtools-gpui ~/.local/bin/`)"
            .to_string()
    })?;

    let mut cmd = std::process::Command::new(&bin);
    if let Some(root) = project_root {
        cmd.arg("--project").arg(root);
    }
    if let Some(p) = page
        && !p.trim().is_empty()
    {
        cmd.arg("--page").arg(p);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                unsafe extern "C" {
                    fn setsid() -> i32;
                }
                let _ = setsid();
                Ok(())
            });
        }
    }

    cmd.spawn()
        .map_err(|e| format!("Failed to launch `{}`: {e}", bin.display()))?;

    let page_suffix = page.map(|p| format!(" (page: {p})")).unwrap_or_default();
    Ok(format!(
        "Launched Native GPUI Flutter DevTools{page_suffix}"
    ))
}

fn resolve_native_devtools_binary() -> Option<PathBuf> {
    const BIN_NAME: &str = "flutter-devtools-gpui";

    // 1. Sibling binary in the same directory as `flutter-debugger-server`
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join(BIN_NAME);
        if sibling.is_file() {
            return Some(sibling);
        }
    }

    // 2. `~/.local/bin/flutter-devtools-gpui`
    if let Some(home) = dirs::home_dir() {
        let local_bin = home.join(".local").join("bin").join(BIN_NAME);
        if local_bin.is_file() {
            return Some(local_bin);
        }
    }

    // 3. `$PATH` lookup
    if let Some(path_os) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_os) {
            let candidate = dir.join(BIN_NAME);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

async fn cmd_overlay(args: &[String]) -> Result<(), String> {
    let project_flag = parse_project_flag(args);
    let name = first_positional_arg(args).ok_or_else(|| {
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

    send_simple_session_cmd(project_flag.as_deref(), IpcRequest::Screenshot { output_path }).await
}

async fn fetch_devices_live(root: &Path) -> Result<Vec<FlutterDevice>, String> {
    let custom_sdk = std::env::var("FLUTTER_SDK_PATH").ok();
    let exe = resolve_sdk_executable(root, ProjectKind::Flutter, custom_sdk.as_deref());
    let out = build_command(&exe, ["devices", "--machine"], root)
        .stdin(Stdio::null())
        .output()
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

/// Drives `flutter run --machine` over structured JSON-RPC while rendering rich ANSI-colored
/// output in Zed's native terminal, measuring real Hot Reload/Restart latency, supporting all
/// Flutter Service Extensions, auto-opening Profile DevTools, and cleaning up on Ctrl+C / SIGTERM.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedRunArgs {
    project_flag: Option<PathBuf>,
    mode: String,
    target: String,
    flavor: Option<String>,
    dart_define_from_file: Option<String>,
    device_flag: Option<String>,
    extra_args: Vec<String>,
}

fn parse_run_args(args: &[String]) -> ParsedRunArgs {
    let dashdash_idx = args.iter().position(|a| a == "--");
    let ctl_args = match dashdash_idx {
        Some(idx) => &args[..idx],
        None => args,
    };
    let extra_args = dashdash_idx
        .map(|idx| args[idx + 1..].to_vec())
        .unwrap_or_default();

    ParsedRunArgs {
        project_flag: parse_project_flag(ctl_args),
        mode: parse_flag_value(ctl_args, "--mode", Some("-m"))
            .unwrap_or_else(|| "debug".to_string())
            .to_ascii_lowercase(),
        target: parse_flag_value(ctl_args, "--target", Some("-t"))
            .unwrap_or_else(|| "lib/main.dart".into()),
        flavor: parse_flag_value(ctl_args, "--flavor", None),
        dart_define_from_file: parse_flag_value(ctl_args, "--dart-define-from-file", None),
        device_flag: parse_flag_value(ctl_args, "--device", Some("-d")),
        extra_args,
    }
}

async fn cmd_run(args: &[String]) -> Result<(), String> {
    let parsed = parse_run_args(args);
    let project = resolve_project_info(parsed.project_flag.as_deref())?;
    if project.kind != ProjectKind::Flutter {
        return Err("`ctl run` is only supported for Flutter projects.".to_string());
    }

    let mode = parsed.mode;
    let target = parsed.target;
    let flavor = parsed.flavor;
    let dart_define_from_file = parsed.dart_define_from_file;
    let cache = DiskCache::default_location();
    let extra_args = parsed.extra_args;
    let (device_id, pass_device_flag) = if let Some(extra_dev) = extract_device_flag_from_args(&extra_args) {
        (Some(extra_dev), false)
    } else if let Some(resolved_dev) = resolve_effective_device(&cache, &project.root, parsed.device_flag.as_deref()) {
        if let Some(notice) = resolved_dev.auto_selected_notice {
            println!("\x1b[33m[Flutter]\x1b[0m {notice}");
        }
        (Some(resolved_dev.id), true)
    } else {
        (None, false)
    };

    let mode_flag = match mode.as_str() {
        "profile" => "--profile",
        "release" => "--release",
        _ => "--debug",
    };

    let mut flutter_args = vec![
        "run".to_string(),
        "--machine".to_string(),
        mode_flag.to_string(),
        "-t".to_string(),
        target.clone(),
    ];
    if pass_device_flag
        && let Some(ref dev) = device_id
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

    println!(
        "\x1b[1;36m[Flutter]\x1b[0m Launching \x1b[1m{target}\x1b[0m ({mode}) on {}...",
        device_id.as_deref().unwrap_or("auto")
    );

    let mut child = build_command(&exe, &flutter_args, &project.root)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
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
        started_at: now_unix(),
        project_root: project.root.clone(),
        device_id: device_id.clone(),
        mode: mode.clone(),
        app_id: None,
        vm_service_uri: None,
        devtools_uri: None,
        overlays: OverlayState::default(),
        exception_pause_mode: ExceptionPauseMode::None,
        debug_library_scope: DebugLibraryScope::MyCodeOnly,
        pending_rpc: HashMap::new(),
    }));
    let next_rpc_id = Arc::new(AtomicI64::new(1));

    persist_run_session(&cache, ipc_port, &run_state).await;
    let _cleanup = RunSessionCleanupGuard {
        cache: cache.clone(),
        session_id,
    };

    let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(64);
    let (stop_tx, mut stop_rx) = mpsc::channel::<()>(4);

    let stdin_writer = tokio::spawn(async move {
        let mut writer = child_stdin;
        while let Some(line) = stdin_rx.recv().await {
            if writer.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            let _ = writer.flush().await;
        }
    });

    // Forward stderr with dim red coloring
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(child_stderr);
        let mut buf = String::new();
        while reader.read_line(&mut buf).await.unwrap_or(0) > 0 {
            let cleaned = strip_ansi_codes(&buf);
            if !cleaned.trim().is_empty() {
                eprint!("\x1b[31m{cleaned}\x1b[0m");
                let _ = io::stderr().flush();
            }
            buf.clear();
        }
    });

    // Parse `flutter run --machine` JSON-RPC events & responses on stdout
    let out_state = Arc::clone(&run_state);
    let out_cache = cache.clone();
    let out_sdk = custom_sdk.clone();
    let stdout_task = tokio::spawn(async move {
        let mut reader = BufReader::new(child_stdout);
        let mut buf = String::new();
        let mut profile_devtools_opened = false;

        while reader.read_line(&mut buf).await.unwrap_or(0) > 0 {
            let cleaned = strip_ansi_codes(&buf);
            if let Some(messages) = parse_machine_line(&cleaned) {
                for msg in messages {
                    // Check if this is an RPC response matching a pending request
                    if let Some(id) = msg.get("id").and_then(Value::as_i64)
                        && let Some(tx) = out_state.lock().await.pending_rpc.remove(&id)
                    {
                        let _ = tx.send(msg);
                        continue;
                    }

                    let event = msg
                        .get("event")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let params = msg.get("params");

                    match event {
                        "app.start" => {
                            if let Some(app_id) =
                                params.and_then(|p| p.get("appId")).and_then(Value::as_str)
                            {
                                let dev_id = params
                                    .and_then(|p| p.get("deviceId"))
                                    .and_then(Value::as_str)
                                    .map(ToOwned::to_owned);
                                {
                                    let mut g = out_state.lock().await;
                                    g.app_id = Some(app_id.to_string());
                                    if g.device_id.is_none() {
                                        g.device_id = dev_id;
                                    }
                                }
                                persist_run_session(&out_cache, ipc_port, &out_state).await;
                            }
                        }
                        "app.debugPort" => {
                            if let Some(ws_uri) =
                                params.and_then(|p| p.get("wsUri")).and_then(Value::as_str)
                            {
                                let (root, mode_now, dt_now) = {
                                    let mut g = out_state.lock().await;
                                    g.vm_service_uri = Some(ws_uri.to_string());
                                    (g.project_root.clone(), g.mode.clone(), g.devtools_uri.clone())
                                };
                                persist_run_session(&out_cache, ipc_port, &out_state).await;
                                println!("\x1b[32m[VM Service]\x1b[0m {ws_uri}");

                                if mode_now == "profile" && !profile_devtools_opened {
                                    profile_devtools_opened = true;
                                    if let Some(url) = resolve_or_start_devtools_url(
                                        &out_cache,
                                        &root,
                                        dt_now.as_deref(),
                                        Some(ws_uri),
                                        Some("performance"),
                                        out_sdk.as_deref(),
                                    )
                                    .await
                                    {
                                        open_url_in_browser(&url);
                                        println!("\x1b[35m[DevTools]\x1b[0m Opened Performance: {url}");
                                    }
                                }
                            }
                        }
                        "app.webLaunchUrl" => {
                            if let Some(url) =
                                params.and_then(|p| p.get("url")).and_then(Value::as_str)
                            {
                                println!("\x1b[36m[Web]\x1b[0m {url}");
                            }
                        }
                        "app.started" => {
                            println!(
                                "\x1b[1;32m[Flutter]\x1b[0m Application running. \x1b[2mPress r (Hot Reload), R (Hot Restart), d (DevTools), q (Quit) + Enter\x1b[0m"
                            );
                        }
                        "app.progress" => {
                            if let Some(message) =
                                params.and_then(|p| p.get("message")).and_then(Value::as_str)
                            {
                                println!("\x1b[36m[Progress]\x1b[0m {message}");
                            }
                        }
                        "app.log" => {
                            if let Some(log) =
                                params.and_then(|p| p.get("log")).and_then(Value::as_str)
                            {
                                let is_err = params
                                    .and_then(|p| p.get("error"))
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false);
                                if is_err {
                                    println!("\x1b[31m{log}\x1b[0m");
                                } else {
                                    println!("{log}");
                                }
                            }
                        }
                        "daemon.logMessage" => {
                            if let Some(message) =
                                params.and_then(|p| p.get("message")).and_then(Value::as_str)
                            {
                                println!("\x1b[2m{message}\x1b[0m");
                            }
                        }
                        _ => {}
                    }
                }
            } else if !cleaned.trim().is_empty() {
                print!("{cleaned}");
                let _ = io::stdout().flush();
            }
            buf.clear();
        }
    });

    // Interactive keyboard controls (r / R / d / q) routed through the exact same RPC engine
    let kb_state = Arc::clone(&run_state);
    let kb_stdin = stdin_tx.clone();
    let kb_rpc_id = Arc::clone(&next_rpc_id);
    let kb_cache = cache.clone();
    let kb_sdk = custom_sdk.clone();
    let kb_stop = stop_tx.clone();
    let term_reader = tokio::spawn(async move {
        let mut stdin = BufReader::new(tokio::io::stdin());
        let mut line = String::new();
        while stdin.read_line(&mut line).await.unwrap_or(0) > 0 {
            match line.trim() {
                "r" => {
                    let resp =
                        execute_machine_restart(&kb_state, &kb_stdin, &kb_rpc_id, false).await;
                    print_reload_banner(&resp);
                }
                "R" => {
                    let resp =
                        execute_machine_restart(&kb_state, &kb_stdin, &kb_rpc_id, true).await;
                    print_reload_banner(&resp);
                }
                "d" => {
                    let resp = execute_open_devtools(&kb_state, &kb_cache, &kb_sdk, None).await;
                    println!("\x1b[35m[DevTools]\x1b[0m {}", resp.message);
                }
                "q" | "Q" => {
                    let _ = kb_stop.send(()).await;
                    break;
                }
                _ => {}
            }
            line.clear();
        }
    });

    // Local IPC server for LSP & `ctl` subcommands
    let ipc_state = Arc::clone(&run_state);
    let ipc_stdin = stdin_tx.clone();
    let ipc_rpc_id = Arc::clone(&next_rpc_id);
    let ipc_cache = cache.clone();
    let ipc_sdk = custom_sdk.clone();
    let ipc_stop = stop_tx.clone();
    let ipc_task = tokio::spawn(async move {
        while let Ok((stream, _)) = ipc_listener.accept().await {
            let st = Arc::clone(&ipc_state);
            let tx = ipc_stdin.clone();
            let rid = Arc::clone(&ipc_rpc_id);
            let c = ipc_cache.clone();
            let sdk = ipc_sdk.clone();
            let stop = ipc_stop.clone();
            tokio::spawn(async move {
                handle_run_ipc(stream, st, tx, rid, c, sdk, stop).await;
            });
        }
    });

    // Wait for child exit, Ctrl+C, or IPC/keyboard stop request
    let exit_status = tokio::select! {
        status = child.wait() => status.ok(),
        _ = wait_for_termination_signal() => {
            println!("\n\x1b[33m[Flutter]\x1b[0m Stopping application...");
            graceful_stop_machine_app(&run_state, &stdin_tx, &next_rpc_id, &mut child).await;
            None
        }
        _ = stop_rx.recv() => {
            println!("\x1b[33m[Flutter]\x1b[0m Stopping application...");
            graceful_stop_machine_app(&run_state, &stdin_tx, &next_rpc_id, &mut child).await;
            None
        }
    };

    ipc_task.abort();
    term_reader.abort();
    drop(stdin_tx);
    let _ = stdin_writer.await;
    let _ = stdout_task.await;
    let _ = stderr_task.await;

    match exit_status {
        None => Ok(()),
        Some(st) if st.success() => Ok(()),
        Some(st) => Err(format!(
            "flutter run exited with status {}",
            st.code().unwrap_or(-1)
        )),
    }
}

async fn wait_for_termination_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut sigterm) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = sigterm.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

async fn graceful_stop_machine_app(
    state: &Arc<Mutex<RunSessionState>>,
    stdin_tx: &mpsc::Sender<String>,
    next_rpc_id: &Arc<AtomicI64>,
    child: &mut tokio::process::Child,
) {
    let app_id = state.lock().await.app_id.clone();
    if let Some(app_id) = app_id {
        let id = next_rpc_id.fetch_add(1, Ordering::Relaxed);
        let payload = format!(
            "{}\n",
            json!([{ "method": "app.stop", "id": id, "params": { "appId": app_id } }])
        );
        let _ = stdin_tx.send(payload).await;
    }

    if tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .is_err()
    {
        terminate_child_tree(child).await;
    }
}

fn print_reload_banner(resp: &IpcResponse) {
    if resp.ok {
        println!("\x1b[32m[Flutter]\x1b[0m {}", resp.message);
    } else {
        println!("\x1b[31m[Flutter]\x1b[0m {}", resp.message);
    }
}

struct RunSessionState {
    session_id: String,
    started_at: u64,
    project_root: PathBuf,
    device_id: Option<String>,
    mode: String,
    app_id: Option<String>,
    vm_service_uri: Option<String>,
    devtools_uri: Option<String>,
    overlays: OverlayState,
    exception_pause_mode: ExceptionPauseMode,
    debug_library_scope: DebugLibraryScope,
    pending_rpc: HashMap<i64, oneshot::Sender<Value>>,
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
        started_at: guard.started_at,
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

async fn send_machine_rpc(
    state: &Arc<Mutex<RunSessionState>>,
    stdin_tx: &mpsc::Sender<String>,
    next_rpc_id: &Arc<AtomicI64>,
    method: &str,
    params: Value,
    timeout_secs: u64,
) -> Result<(Value, u64), String> {
    let start = Instant::now();
    let id = next_rpc_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    state.lock().await.pending_rpc.insert(id, tx);

    let line = format!(
        "{}\n",
        json!([{
            "method": method,
            "id": id,
            "params": params
        }])
    );

    if stdin_tx.send(line).await.is_err() {
        state.lock().await.pending_rpc.remove(&id);
        return Err("Flutter run session is closed".to_string());
    }

    match tokio::time::timeout(Duration::from_secs(timeout_secs), rx).await {
        Ok(Ok(resp)) => {
            let ms = start.elapsed().as_millis() as u64;
            if let Some(err) = resp.get("error") {
                let err_msg = err
                    .as_str()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| err.to_string());
                return Err(err_msg);
            }
            Ok((resp.get("result").cloned().unwrap_or(Value::Null), ms))
        }
        Ok(Err(_)) => Err("Flutter run session closed before responding".to_string()),
        Err(_) => {
            state.lock().await.pending_rpc.remove(&id);
            Err(format!("Timed out waiting for `{method}` response"))
        }
    }
}

async fn execute_machine_restart(
    state: &Arc<Mutex<RunSessionState>>,
    stdin_tx: &mpsc::Sender<String>,
    next_rpc_id: &Arc<AtomicI64>,
    full_restart: bool,
) -> IpcResponse {
    let (mode, dev, vm, dt, app_id) = {
        let g = state.lock().await;
        (
            g.mode.clone(),
            g.device_id.clone(),
            g.vm_service_uri.clone(),
            g.devtools_uri.clone(),
            g.app_id.clone(),
        )
    };
    let label = if full_restart {
        "Hot Restart"
    } else {
        "Hot Reload"
    };
    if mode != "debug" {
        return IpcResponse::err(format!("{label} is disabled in `{mode}` mode"));
    }
    let Some(app_id) = app_id else {
        return IpcResponse::err("Flutter app is still starting; try again in a moment");
    };

    match send_machine_rpc(
        state,
        stdin_tx,
        next_rpc_id,
        "app.restart",
        json!({
            "appId": app_id,
            "fullRestart": full_restart,
            "pause": false
        }),
        15,
    )
    .await
    {
        Ok((result, ms)) => {
            let code = result.get("code").and_then(Value::as_i64).unwrap_or(0);
            if code == 0 {
                IpcResponse {
                    ok: true,
                    message: format!("{label}: {ms}ms"),
                    duration_ms: Some(ms),
                    mode: Some(mode),
                    device_id: dev,
                    vm_service_uri: vm,
                    devtools_uri: dt,
                    ..Default::default()
                }
            } else {
                let err_msg = result
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Reload rejected (compilation error)");
                IpcResponse {
                    ok: false,
                    message: format!("{label} failed: {err_msg}"),
                    duration_ms: Some(ms),
                    mode: Some(mode),
                    device_id: dev,
                    vm_service_uri: vm,
                    devtools_uri: dt,
                    ..Default::default()
                }
            }
        }
        Err(err) => IpcResponse::err(format!("{label} failed: {err}")),
    }
}

async fn execute_open_devtools(
    state: &Arc<Mutex<RunSessionState>>,
    cache: &DiskCache,
    custom_sdk: &Option<String>,
    page: Option<&str>,
) -> IpcResponse {
    let (root, dt, vm) = {
        let g = state.lock().await;
        (
            g.project_root.clone(),
            g.devtools_uri.clone(),
            g.vm_service_uri.clone(),
        )
    };
    if let Some(url) = resolve_or_start_devtools_url(
        cache,
        &root,
        dt.as_deref(),
        vm.as_deref(),
        page,
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
            ..Default::default()
        }
    } else {
        IpcResponse::err("DevTools URI is not ready yet")
    }
}

async fn handle_run_ipc(
    mut stream: tokio::net::TcpStream,
    state: Arc<Mutex<RunSessionState>>,
    stdin_tx: mpsc::Sender<String>,
    next_rpc_id: Arc<AtomicI64>,
    cache: DiskCache,
    custom_sdk: Option<String>,
    stop_tx: mpsc::Sender<()>,
) {
    let (read_half, mut write_half) = stream.split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }

    let response = match serde_json::from_str::<IpcRequest>(line.trim()) {
        Ok(IpcRequest::HotReload) => {
            let resp = execute_machine_restart(&state, &stdin_tx, &next_rpc_id, false).await;
            print_reload_banner(&resp);
            resp
        }
        Ok(IpcRequest::HotRestart) => {
            let resp = execute_machine_restart(&state, &stdin_tx, &next_rpc_id, true).await;
            print_reload_banner(&resp);
            resp
        }
        Ok(IpcRequest::Stop) => {
            let session_id = state.lock().await.session_id.clone();
            remove_session_record_by_id(&cache, &session_id);
            let resp = IpcResponse {
                ok: true,
                message: "Stopping Flutter application...".to_string(),
                ..Default::default()
            };
            write_ipc_resp(&mut write_half, &resp).await;
            let _ = stop_tx.send(()).await;
            return;
        }
        Ok(IpcRequest::ToggleOverlay { overlay }) => {
            let (mode, dev, vm, dt, app_id, method, params, label) = {
                let g = state.lock().await;
                if g.mode == "release" {
                    write_ipc_resp(
                        &mut write_half,
                        &IpcResponse::err("Visual overlays are unavailable in release mode"),
                    )
                    .await;
                    return;
                }
                let (m, p, l) = g.overlays.preview_toggle(overlay);
                (
                    g.mode.clone(),
                    g.device_id.clone(),
                    g.vm_service_uri.clone(),
                    g.devtools_uri.clone(),
                    g.app_id.clone(),
                    m,
                    p,
                    l,
                )
            };

            let rpc_ok = if let Some(aid) = app_id {
                send_machine_rpc(
                    &state,
                    &stdin_tx,
                    &next_rpc_id,
                    "app.callServiceExtension",
                    json!({
                        "appId": aid,
                        "methodName": method,
                        "params": params
                    }),
                    8,
                )
                .await
                .ok()
            } else {
                None
            };

            if let Some((_, ms)) = rpc_ok {
                state.lock().await.overlays.toggle(overlay);
                IpcResponse {
                    ok: true,
                    message: label,
                    duration_ms: Some(ms),
                    mode: Some(mode),
                    device_id: dev,
                    vm_service_uri: vm,
                    devtools_uri: dt,
                    ..Default::default()
                }
            } else if let Some(vm_uri) = vm.as_deref()
                && let Ok(mut client) = VmServiceClient::connect(vm_uri).await
                && client.call_service_extension(method, params).await.is_ok()
            {
                state.lock().await.overlays.toggle(overlay);
                IpcResponse {
                    ok: true,
                    message: label,
                    duration_ms: None,
                    mode: Some(mode),
                    device_id: dev,
                    vm_service_uri: vm,
                    devtools_uri: dt,
                    ..Default::default()
                }
            } else {
                IpcResponse::err(format!("Failed to toggle {}", overlay.label()))
            }
        }
        Ok(IpcRequest::CallServiceExtension { method, params }) => {
            let (app_id, vm) = {
                let g = state.lock().await;
                (g.app_id.clone(), g.vm_service_uri.clone())
            };
            if let Some(aid) = app_id
                && send_machine_rpc(
                    &state,
                    &stdin_tx,
                    &next_rpc_id,
                    "app.callServiceExtension",
                    json!({
                        "appId": aid,
                        "methodName": method,
                        "params": params
                    }),
                    8,
                )
                .await
                .is_ok()
            {
                IpcResponse {
                    ok: true,
                    message: format!("Called `{method}`"),
                    vm_service_uri: vm,
                    ..Default::default()
                }
            } else if let Some(vm_uri) = vm.as_deref()
                && let Ok(mut client) = VmServiceClient::connect(vm_uri).await
                && client.call_service_extension(&method, params).await.is_ok()
            {
                IpcResponse {
                    ok: true,
                    message: format!("Called `{method}`"),
                    vm_service_uri: vm,
                    ..Default::default()
                }
            } else {
                IpcResponse::err(format!("Service extension `{method}` failed"))
            }
        }
        Ok(IpcRequest::OpenDevTools { page }) => {
            execute_open_devtools(&state, &cache, &custom_sdk, page.as_deref()).await
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
                    vm_service_uri: vm,
                    ..Default::default()
                }
            } else {
                IpcResponse::err("Could not capture screenshot via VM Service")
            }
        }
        Ok(IpcRequest::SetExceptionPauseMode { mode }) => {
            let vm = state.lock().await.vm_service_uri.clone();
            if let Some(vm_uri) = vm.as_deref()
                && let Ok(mut client) = VmServiceClient::connect(vm_uri).await
                && let Ok(isolate_id) = client.get_main_isolate_id().await
                && client
                    .call(
                        "setIsolatePauseMode",
                        json!({
                            "isolateId": isolate_id,
                            "exceptionPauseMode": mode.vm_service_mode()
                        }),
                    )
                    .await
                    .is_ok()
            {
                let mut g = state.lock().await;
                g.exception_pause_mode = mode;
                IpcResponse {
                    ok: true,
                    message: format!("Exception pause mode: {}", mode.description()),
                    exception_pause_mode: Some(mode),
                    debug_library_scope: Some(g.debug_library_scope),
                    ..Default::default()
                }
            } else {
                IpcResponse::err(
                    "Exception breakpoints require an active DAP Debug session (or VM Service connection)",
                )
            }
        }
        Ok(IpcRequest::SetDebugLibraryScope { scope }) => {
            let mut g = state.lock().await;
            g.debug_library_scope = scope;
            IpcResponse {
                ok: true,
                message: format!(
                    "Debug library scope set to {} (applies to DAP breakpoint stepping)",
                    scope.description()
                ),
                exception_pause_mode: Some(g.exception_pause_mode),
                debug_library_scope: Some(scope),
                ..Default::default()
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
                exception_pause_mode: Some(g.exception_pause_mode),
                debug_library_scope: Some(g.debug_library_scope),
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
        let _ = write_half.flush().await;
    }
}

pub fn parse_machine_line(line: &str) -> Option<Vec<Value>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return None;
    }
    serde_json::from_str::<Vec<Value>>(trimmed).ok()
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
    let status = build_command(
        &dart_exe,
        ["run", "build_runner", action, "--delete-conflicting-outputs"],
        &project.root,
    )
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

    let status = build_command(&exe, subargs, &root)
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

    let status = build_command(
        &exe,
        [
            "create",
            "--project-name",
            &name,
            "--org",
            &org,
            &format!("--template={template}"),
            &target.to_string_lossy(),
        ],
        &base_dir,
    )
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
        "hotReloadDebounceMs": 250,
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
    fn parses_machine_protocol_lines_and_ignores_plain_text() {
        assert!(parse_machine_line("Launching lib/main.dart...").is_none());
        let events = parse_machine_line(
            r#"[{"event":"app.debugPort","params":{"appId":"1","wsUri":"ws://localhost:50000/ws"}}]"#,
        )
        .expect("parsed");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event"], "app.debugPort");
    }

    #[test]
    fn parses_project_and_overlay_positional_arg() {
        let args = vec![
            "--project".to_string(),
            "/path/to/app".to_string(),
            "inspector".to_string(),
        ];
        assert_eq!(
            parse_project_flag(&args),
            Some(PathBuf::from("/path/to/app"))
        );
        assert_eq!(first_positional_arg(&args), Some("inspector"));
        assert_eq!(
            ServiceOverlay::parse(first_positional_arg(&args).unwrap()),
            Some(ServiceOverlay::Inspector)
        );
    }

    #[test]
    fn run_args_stops_flag_parsing_at_double_dash() {
        let args: Vec<String> = vec![
            "--project",
            "/tmp/app",
            "--mode",
            "profile",
            "--",
            "--mode",
            "release",
            "--device",
            "chrome",
            "--target",
            "lib/other.dart",
            "--dart-define=FOO=1",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let parsed = parse_run_args(&args);
        assert_eq!(parsed.project_flag, Some(PathBuf::from("/tmp/app")));
        assert_eq!(parsed.mode, "profile");
        assert_eq!(parsed.target, "lib/main.dart");
        assert_eq!(parsed.device_flag, None);
        assert_eq!(
            parsed.extra_args,
            vec![
                "--mode",
                "release",
                "--device",
                "chrome",
                "--target",
                "lib/other.dart",
                "--dart-define=FOO=1",
            ]
        );
    }

    #[test]
    fn explicit_project_flag_never_falls_back_to_other_project_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::with_root(dir.path().join(".cache"));
        let proj_a = dir.path().join("app_a");
        let proj_b = dir.path().join("app_b");
        fs::create_dir_all(&proj_a).expect("mkdir a");
        fs::create_dir_all(&proj_b).expect("mkdir b");

        let rec = ActiveSessionRecord {
            session_id: "sess-a".into(),
            project_root: normalize_project_key(&proj_a),
            device_id: Some("macos".into()),
            mode: "debug".into(),
            pid: std::process::id(),
            ipc_port: 41234,
            vm_service_uri: None,
            devtools_uri: None,
            started_at: 100,
            is_test: false,
            kind: "run".into(),
        };
        write_session_record(&cache, &rec).expect("write");

        // Explicit --project app_b must NOT fall back to app_a
        assert!(resolve_session_for_ctl(&cache, Some(&proj_b), &proj_b).is_none());
        // Omitted --project falls back to latest active session
        assert_eq!(
            resolve_session_for_ctl(&cache, None, &proj_b).map(|s| s.session_id),
            Some("sess-a".to_string())
        );
    }
}
