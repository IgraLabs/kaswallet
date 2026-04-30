use crate::address_manager::AddressManager;
use crate::args::Args;
use crate::service::kaswallet_service::KasWalletService;
use crate::sync_manager::SyncManager;
use crate::transaction_generator::TransactionGenerator;
use crate::{kaspad_client, utxo_manager};
use common::args::calculate_path;
use common::error_location::ErrorLocation;
use common::errors::{RpcError, WalletError, WalletResult};
use common::keys::Keys;
use kaspa_bip32::Prefix;
use kaspa_consensus_core::config::params::Params;
use kaspa_grpc_client::GrpcClient;
use kaspa_rpc_core::api::rpc::RpcApi;
use kaspa_wallet_core::tx::MassCalculator;
use log::{debug, info};
use proto::kaswallet_proto::wallet_server::WalletServer;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tonic::transport::Server;

pub struct Daemon {
    args: Arc<Args>,
}

impl Daemon {
    pub fn new(args: Arc<Args>) -> Self {
        Self { args }
    }

    pub async fn start(&self) -> WalletResult<(JoinHandle<()>, JoinHandle<()>)> {
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
    ) -> WalletResult<(JoinHandle<()>, JoinHandle<()>)> {
        let network_id = self.args.network_id();

        let extended_keys_prefix = Prefix::from(network_id);
        let keys_file_path = calculate_path(&self.args.keys_file_path, &network_id, "keys.json");
        debug!("Keys file path: {}", keys_file_path);
        let keys = Arc::new(Keys::load(&keys_file_path, extended_keys_prefix)?);
        info!("Loaded keys from file {}", keys_file_path);
        let mass_calculator = Arc::new(MassCalculator::new(&network_id.network_type.into()));

        let block_dag_info = kaspa_rpc_client.get_block_dag_info().await.map_err(|e| {
            WalletError::from(RpcError::Transport {
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            })
        })?;

        let address_prefix = network_id.network_type.into();
        let address_manager = Arc::new(Mutex::new(AddressManager::new(
            keys.clone(),
            address_prefix,
        )));
        let utxo_manager = Arc::new(Mutex::new(utxo_manager::UtxoManager::new(
            address_manager.clone(),
            consensus_params,
            block_dag_info,
        )));
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
