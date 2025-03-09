use crate::address_manager::AddressManager;
use clap::Parser;
use common::args::expand_path;
use common::keys::Keys;
use kaspa_bip32::Prefix;
use ::log::{error, info};
use std::error::Error;
use std::sync::Arc;
use tonic::transport::Server;
use wallet_proto::wallet_proto::wallet_server::WalletServer;

mod address_manager;
mod args;
mod kaspad_client;
mod log;
mod model;
mod service;
mod sync;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = args::Args::parse();

    if let Err(e) = log::init_log(args.logs_path.clone(), args.logs_level.clone()) {
        panic!("Failed to initialize logger: {}", e);
    }

    let prefix = Prefix::from(args.network());
    let keys_file_path = expand_path(args.keys_file.clone());
    let keys = Keys::load(keys_file_path.clone(), prefix);
    if let Err(e) = keys {
        error!("Failed to load keys from file {}: {}", keys_file_path, e);
        error!("Please run kaswallet-create or provide a `--keys-file` flag");
        return Ok(());
    }
    let keys = Arc::new(keys.unwrap());
    info!("Loaded keys from file {}", keys_file_path);

    let kaspa_rpc_client = kaspad_client::connect(args.server.clone(), args.network()).await?;

    let prefix = args.network().network_type.into();
    let address_manager = Arc::new(AddressManager::new(
        kaspa_rpc_client.clone(),
        keys.clone(),
        prefix,
    ));
    let service =
        service::KasWalletService::new(args.clone(), kaspa_rpc_client, address_manager, keys);
    let server = WalletServer::new(service);

    info!("Starting wallet server on {}", args.listen);

    Server::builder()
        .add_service(server)
        .serve(args.listen.parse().unwrap())
        .await?;

    Ok(())
}
