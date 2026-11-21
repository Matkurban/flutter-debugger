mod app_runner;
mod cache;
mod dap;
mod debug_config;
mod emulators;
mod ipc;
mod lsp;
mod project;
mod runner;
mod wizard;

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
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
        Some("--version" | "-V") => {
            println!("flutter-debugger-server {}", env!("CARGO_PKG_VERSION"));
        }
        other => {
            eprintln!(
                "flutter-debugger-server {}\nUsage: flutter-debugger-server <lsp|dap|--version>\nUnknown argument: {:?}",
                env!("CARGO_PKG_VERSION"),
                other
            );
            std::process::exit(2);
        }
    }
}
