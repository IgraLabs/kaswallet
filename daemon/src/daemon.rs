use crate::address_manager::AddressManager;
use crate::args::Args;
use crate::daemon::DaemonStartError::{FailedToLoadKeys, RpcError};
use crate::service::kaswallet_service::KasWalletService;
use crate::sync_manager::SyncManager;
use crate::transaction_generator::TransactionGenerator;
use crate::{kaspad_client, utxo_manager};
use common::args::calculate_path;
use common::keys::Keys;
use kaspa_bip32::Prefix;
use kaspa_consensus_core::config::params::Params;
use kaspa_grpc_client::GrpcClient;
use kaspa_rpc_core::api::rpc::RpcApi;
use kaspa_wallet_core::tx::MassCalculator;
use log::{debug, info};
use proto::kaswallet_proto::wallet_server::WalletServer;
use std::error::Error;
use std::sync::Arc;
use thiserror::Error as ThisError;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tonic::transport::Server;

pub struct Daemon {
    args: Arc<Args>,
}

#[derive(ThisError, Debug)]
pub enum DaemonStartError {
    #[error(
        "Failed to load keys from file {0}: {1} \nPlease run kaswallet-create or provide a `--keys-file` flag"
    )]
    FailedToLoadKeys(String, Box<dyn Error + Send + Sync>),
    #[error("Failed to connect to kaspad at {0}: {1}")]
    FailedToConnectToKaspad(String, kaspa_grpc_client::error::Error),
    #[error("RPC error: {0}")]
    RpcError(kaspa_rpc_core::RpcError),
}

pub type DaemonStartResult<T> = Result<T, DaemonStartError>;

impl Daemon {
    pub fn new(args: Arc<Args>) -> Self {
        Self { args }
    }

    pub async fn start(&self) -> DaemonStartResult<(JoinHandle<()>, JoinHandle<()>)> {
        let network_id = self.args.network_id();
        let kaspa_rpc_client =
            Arc::new(kaspad_client::connect(&self.args.server, &network_id).await?);
        let consensus_params = Params::from(network_id.network_type);

        self.start_with_kaspad_client_and_consensus_params(kaspa_rpc_client, consensus_params)
            .await
    }

    pub async fn start_with_kaspad_client_and_consensus_params(
        &self,
        kaspa_rpc_client: Arc<GrpcClient>,
        consensus_params: Params,
    ) -> DaemonStartResult<(JoinHandle<()>, JoinHandle<()>)> {
        let network_id = self.args.network_id();

        let extended_keys_prefix = Prefix::from(network_id);
        let keys_file_path = calculate_path(&self.args.keys_file_path, &network_id, "keys.json");
        debug!("Keys file path: {}", keys_file_path);
        let keys = Arc::new(
            Keys::load(&keys_file_path, extended_keys_prefix)
                .map_err(|e| FailedToLoadKeys(keys_file_path.clone(), e))?,
        );
        info!("Loaded keys from file {}", keys_file_path);
        let mass_calculator = Arc::new(MassCalculator::new(&network_id.network_type.into()));

        let block_dag_info = kaspa_rpc_client
            .get_block_dag_info()
            .await
            .map_err(RpcError)?;

        let address_prefix = network_id.network_type.into();
        let address_manager = Arc::new(Mutex::new(AddressManager::new(
            keys.clone(),
            address_prefix,
        )));
        let utxo_manager = Arc::new(utxo_manager::UtxoManager::new(
            address_manager.clone(),
            consensus_params,
            block_dag_info,
        ));
        let transaction_generator = Arc::new(Mutex::new(TransactionGenerator::new(
            kaspa_rpc_client.clone(),
            keys.clone(),
            address_manager.clone(),
            mass_calculator.clone(),
            address_prefix,
        )));
        let sync_manager = Arc::new(SyncManager::new(
            kaspa_rpc_client.clone(),
            keys.clone(),
            address_manager.clone(),
            utxo_manager.clone(),
            self.args.sync_interval_millis,
        ));
        let sync_manager_handle = SyncManager::start(sync_manager.clone());

        let service = KasWalletService::new(
            kaspa_rpc_client.clone(),
            keys,
            address_manager.clone(),
            utxo_manager.clone(),
            transaction_generator.clone(),
            sync_manager.clone(),
        );

        let listen = self.args.listen.clone();
        let server_handle = tokio::spawn(async move {
            info!("Starting wallet server on {}", listen);
            let server = WalletServer::new(service);
            let serve_result = Server::builder()
                .add_service(server)
                .serve(listen.parse().unwrap())
                .await;

            if let Err(e) = serve_result {
                panic!("Error from server: {}", e);
            }
        });
        Ok((sync_manager_handle, server_handle))
    }
}
