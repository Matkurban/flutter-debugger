#![cfg_attr(not(test), deny(clippy::unwrap_used))]

mod backend;
mod cli;
mod protocol;
mod single_instance;
mod theme;
mod ui;

use backend::{BackendCommand, spawn_backend_worker};
use cli::{CliLaunchArgs, print_usage};
use gpui_kit::{
    AppContext as _, Bounds, KeyBinding, WindowBounds, WindowOptions,
    assets::Assets,
    component::{Theme, ThemeMode},
    px, size,
};
use protocol::DevToolsPage;
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

    let (single_instance_rx, _instance_guard) =
        match try_forward_or_bind_single_instance(&SingleInstanceFocusCommand {
            action: "focus".to_string(),
            page: launch_args.page.map(|p| p.as_slug().to_string()),
            project: launch_args.project.clone(),
            session_id: launch_args.session_id.clone(),
            vm_uri: launch_args.vm_uri.clone(),
        }) {
            Ok(Some((rx, guard))) => (rx, Some(guard)),
            Ok(None) => return,
            Err(err) => {
                eprintln!("[flutter-devtools-gpui] single-instance warning: {err}");
                let (_, rx) = async_channel::unbounded();
                (rx, None)
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
