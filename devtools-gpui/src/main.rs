mod backend;
mod single_instance;
mod theme;
mod ui;

use backend::{BackendCommand, CliLaunchArgs, DevToolsPage, spawn_backend_worker};
use gpui_kit::{
    AppContext as _, Bounds, KeyBinding, WindowBounds, WindowOptions,
    assets::Assets,
    component::{Theme, ThemeMode},
    px, size,
};
use single_instance::{SingleInstanceFocusCommand, try_forward_or_bind_single_instance};
use ui::app::{
    HotReloadAction, HotRestartAction, RootView, SwitchTabInspector, SwitchTabLogging,
    SwitchTabMemory, SwitchTabNetwork, SwitchTabPerformance,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return;
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("flutter-devtools-gpui {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let launch_args = CliLaunchArgs::parse(&args);

    // Single-instance check: if an existing instance is alive, send focus/page/session command and exit.
    let single_instance_rx = match try_forward_or_bind_single_instance(&SingleInstanceFocusCommand {
        action: "focus".to_string(),
        page: launch_args.page.map(|p| p.as_slug().to_string()),
        project: launch_args.project.clone(),
        session_id: launch_args.session_id.clone(),
        vm_uri: launch_args.vm_uri.clone(),
    }) {
        Ok(Some(rx)) => rx,
        Ok(None) => {
            // Existing instance handled the focus request.
            return;
        }
        Err(err) => {
            eprintln!("[flutter-devtools-gpui] single-instance warning: {err}");
            let (_, rx) = async_channel::unbounded();
            rx
        }
    };

    let (cmd_tx, event_rx) = spawn_backend_worker(launch_args.clone());

    gpui_kit::application()
        .with_assets(Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);

            cx.bind_keys([
                KeyBinding::new("cmd-1", SwitchTabInspector, None),
                KeyBinding::new("cmd-2", SwitchTabNetwork, None),
                KeyBinding::new("cmd-3", SwitchTabLogging, None),
                KeyBinding::new("cmd-4", SwitchTabPerformance, None),
                KeyBinding::new("cmd-5", SwitchTabMemory, None),
                KeyBinding::new("cmd-r", HotReloadAction, None),
                KeyBinding::new("cmd-shift-r", HotRestartAction, None),
            ]);

            let bounds = Bounds::centered(None, size(px(1280.0), px(820.0)), cx);
            let window_options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(900.0), px(560.0))),
                focus: true,
                show: true,
                ..Default::default()
            };

            let initial_page = launch_args.page.unwrap_or(DevToolsPage::Network);
            let cmd_tx_clone = cmd_tx.clone();
            let event_rx_clone = event_rx.clone();
            let single_rx_clone = single_instance_rx.clone();

            if let Err(err) = gpui_kit::open_window(window_options, cx, move |window, cx| {
                cx.new(|cx| {
                    RootView::new(
                        initial_page,
                        cmd_tx_clone,
                        event_rx_clone,
                        single_rx_clone,
                        window,
                        cx,
                    )
                })
            }) {
                eprintln!("Failed to open Flutter DevTools GPUI window: {err}");
                let _ = cmd_tx.try_send(BackendCommand::Shutdown);
            }
        });
}

fn print_usage() {
    println!(
        "flutter-devtools-gpui {}
Native GPUI Flutter DevTools companion application for Zed

USAGE:
  flutter-devtools-gpui [OPTIONS]

OPTIONS:
  --project <dir>       Attach to active Flutter session for project directory
  --session <id>        Attach to a specific session ID
  --vm-uri <ws://...>   Directly connect to a Dart VM Service URI
  --page <tab>          Initial tab: inspector | network | logging | performance | memory
  -h, --help            Print help
  -V, --version         Print version",
        env!("CARGO_PKG_VERSION")
    );
}
