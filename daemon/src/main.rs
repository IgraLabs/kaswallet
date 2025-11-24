use ::log::{error, info};
use clap::Parser;
use common::args::calculate_path;
use kaswallet_daemon::{args, daemon::Daemon};
use std::sync::Arc;
use tokio::select;

#[tokio::main]
async fn main() {
    let args = Arc::new(args::Args::parse());

    #[cfg(debug_assertions)]
    {
        if args.enable_tokio_console {
            console_subscriber::init();
        }
    }

    let logs_path = calculate_path(&args.logs_path, &args.network_id(), "logs");
    if let Err(e) = kaswallet_daemon::log::init_log(&logs_path, &args.logs_level) {
        panic!("Failed to initialize logger: {}", e);
    }

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
