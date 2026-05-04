use ::tracing::{error, info};
use clap::Parser;
use common::args::calculate_path;
use kaswallet_daemon::{args, daemon::Daemon};
use std::sync::Arc;
use tokio::select;

#[tokio::main]
async fn main() {
    let args = Arc::new(args::Args::parse());

    #[cfg(debug_assertions)]
    let enable_console = args.enable_tokio_console;
    #[cfg(not(debug_assertions))]
    let enable_console = false;

    let logs_path = calculate_path(&args.logs_path, &args.network_id(), "logs");
    let _log_guards = kaswallet_daemon::log::init_log(&logs_path, &args.logs_level, enable_console)
        .unwrap_or_else(|e| {
            eprintln!("Failed to initialize logger: {}", e.user_message());
            std::process::exit(1);
        });

    let daemon = Daemon::new(args.clone());

    let (sync_manager_handle, server_handle) = match daemon.start().await {
        Err(e) => {
            error!("{}", e);
            return;
        }
        Ok((sync_manager_handle, server_handle)) => (sync_manager_handle, server_handle),
    };

    select! {
        result = sync_manager_handle => {
            if let Err(e) = result {
                panic!("Error from sync manager: {}", e);
            }
            info!("Sync manager has finished");
        }
        result = server_handle => {
            if let Err(e) = result {
                panic!("Error from server: {}", e);
            }
            info!("Server has finished");
        }
    };
}
