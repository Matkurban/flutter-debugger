mod cache;
mod ctl;
mod daemon;
mod dap;
mod debug_config;
mod devtools;
mod emulators;
mod ipc;
mod lsp;
mod process_util;
mod project;
mod runner;
mod vm_service;

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("lsp") => {
            lsp::run_lsp_server().await;
        }
        Some("dap") => {
            if let Err(err) = dap::run_dap_proxy().await {
                tracing::error!("DAP proxy terminated with error: {err}");
                std::process::exit(1);
            }
        }
        Some("ctl") => {
            let rest: Vec<String> = args.collect();
            if let Err(err) = ctl::run_ctl(rest).await {
                eprintln!("{err}");
                std::process::exit(1);
            }
        }
        Some("init-zed") => {
            let rest: Vec<String> = args.collect();
            if let Err(err) = debug_config::run_init_zed(&rest) {
                eprintln!("{err}");
                std::process::exit(1);
            }
        }
        Some("--version" | "-V") => {
            println!("flutter-debugger-server {}", env!("CARGO_PKG_VERSION"));
        }
        other => {
            eprintln!(
                "flutter-debugger-server {}\nUsage: flutter-debugger-server <lsp|dap|ctl|init-zed|--version>\nUnknown argument: {:?}",
                env!("CARGO_PKG_VERSION"),
                other
            );
            std::process::exit(2);
        }
    }
}
