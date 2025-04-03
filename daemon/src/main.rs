use crate::address_manager::AddressManager;
use crate::sync_manager::SyncManager;
use crate::transaction_generator::TransactionGenerator;
use clap::Parser;
use common::args::calculate_path;
use common::keys::Keys;
use kaspa_bip32::Prefix;
use kaspa_wallet_core::tx::MassCalculator;
use kaspa_wallet_core::utxo::NetworkParams;
use kaswallet_proto::kaswallet_proto::wallet_server::WalletServer;
use ::log::{debug, error, info};
use std::error::Error;
use std::sync::Arc;
use tokio::select;
use tokio::sync::Mutex;
use tonic::transport::Server;

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
    #[cfg(debug_assertions)]
    {
        if args.enable_tokio_console {
            console_subscriber::init();
        }
    }

    let network_id = args.network_id();

    let logs_path = calculate_path(args.logs_path.clone(), network_id, "logs");
    if let Err(e) = log::init_log(logs_path, args.logs_level.clone()) {
        panic!("Failed to initialize logger: {}", e);
    }

    let extended_keys_prefix = Prefix::from(network_id);
    let keys_file_path = calculate_path(args.keys_file.clone(), network_id, "keys.json");
    debug!("Keys file path: {}", keys_file_path);
    let keys = Keys::load(&keys_file_path, extended_keys_prefix);

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
