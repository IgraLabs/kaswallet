use crate::address_manager::AddressManager;
use crate::sync_manager::SyncManager;
use crate::transaction_generator::TransactionGenerator;
use clap::Parser;
use common::args::expand_path;
use common::keys::Keys;
use kaspa_bip32::Prefix;
use kaspa_wallet_core::tx::MassCalculator;
use kaspa_wallet_core::utxo::NetworkParams;
use ::log::{error, info};
use std::error::Error;
use std::sync::Arc;
use tokio::select;
use tokio::sync::Mutex;
use tonic::transport::Server;
use wallet_proto::wallet_proto::wallet_server::WalletServer;

mod address_manager;
mod args;
mod kaspad_client;
mod log;
mod model;
mod service;
mod sync_manager;
mod transaction_generator;
mod utxo_manager;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = args::Args::parse();

    if let Err(e) = log::init_log(args.logs_path.clone(), args.logs_level.clone()) {
        panic!("Failed to initialize logger: {}", e);
    }

    let network_id = args.network_id();
    let extended_keys_prefix = Prefix::from(args.network_id());
    let keys_file_path = expand_path(args.keys_file.clone());
    let keys = Keys::load(keys_file_path.clone(), extended_keys_prefix);

    if let Err(e) = keys {
        error!("Failed to load keys from file {}: {}", keys_file_path, e);
        error!("Please run kaswallet-create or provide a `--keys-file` flag");
        return Ok(());
    }
    let keys = Arc::new(keys.unwrap());
    info!("Loaded keys from file {}", keys_file_path);
    let network_params = NetworkParams::from(network_id);
    let mass_calculator = Arc::new(MassCalculator::new(&network_id.network_type.into()));

    let kaspa_rpc_client = kaspad_client::connect(args.server.clone(), args.network_id()).await?;

    let address_prefix = network_id.network_type.into();
    let address_manager = Arc::new(Mutex::new(AddressManager::new(
        kaspa_rpc_client.clone(),
        keys.clone(),
        address_prefix,
    )));
    let utxo_manager = Arc::new(Mutex::new(utxo_manager::UtxoManager::new(
        address_manager.clone(),
        network_params,
    )));
    let transaction_generator = Arc::new(Mutex::new(TransactionGenerator::new(
        kaspa_rpc_client.clone(),
        keys.clone(),
        address_manager.clone(),
        utxo_manager.clone(),
        mass_calculator.clone(),
        address_prefix,
    )));
    let sync_manager = Arc::new(Mutex::new(SyncManager::new(
        kaspa_rpc_client.clone(),
        address_manager.clone(),
        utxo_manager.clone(),
        transaction_generator.clone(),
    )));
    let sync_manager_handle = SyncManager::start(sync_manager.clone());

    let service = service::KasWalletService::new(
        kaspa_rpc_client.clone(),
        keys,
        address_manager.clone(),
        utxo_manager.clone(),
        transaction_generator.clone(),
        sync_manager.clone(),
    );

    let server_handle = tokio::spawn(async move {
        info!("Starting wallet server on {}", args.listen);
        let server = WalletServer::new(service);
        let serve_result = Server::builder()
            .add_service(server)
            .serve(args.listen.parse().unwrap())
            .await;

        if let Err(e) = serve_result {
            panic!("Error from server: {}", e);
        }
    });
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
    }

    Ok(())
}
