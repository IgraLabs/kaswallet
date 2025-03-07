use clap::Parser;
use common::args::expand_path;
use common::keys::load_keys;
use kaspa_wrpc_client::prelude::RpcApi;
use ::log::{error, info};
use std::error::Error;
use std::sync::Arc;
use tonic::transport::Server;
use wallet_proto::wallet_proto::wallet_server::WalletServer;

mod args;
mod kaspad_client;
mod log;
mod service;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = args::Args::parse();

    if let Err(e) = log::init_log(args.logs_path.clone(), args.logs_level.clone()) {
        panic!("Failed to initialize logger: {}", e);
    }

    let keys_file_path = expand_path(args.keys_file.clone());
    let keys = load_keys(keys_file_path.clone());
    if let Err(e) = keys {
        error!("Failed to load keys from file {}: {}", keys_file_path, e);
        error!("Please run kaswallet-create or provide a `--keys-file` flag");
        return Ok(());
    }
    let keys = Arc::new(keys.unwrap());
    info!("Loaded keys from file {}", keys_file_path);

    let kaspa_rpc_client = kaspad_client::connect(args.server.clone(), args.network()).await?;

    let service = service::KasWalletService::new(args.clone(), kaspa_rpc_client, keys);
    let server = WalletServer::new(service);

    info!("Starting wallet server on {}", args.listen);

    Server::builder()
        .add_service(server)
        .serve(args.listen.parse().unwrap())
        .await?;

    Ok(())
}
