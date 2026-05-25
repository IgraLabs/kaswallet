use crate::address_manager::AddressManager;
use crate::args::Args;
use crate::args::resolve_subnetwork_id;
use crate::service::kaswallet_service::KasWalletService;
use crate::sync_manager::SyncManager;
use crate::transaction_generator::TransactionGenerator;
use crate::{kaspad_client, utxo_manager};
use common::args::calculate_path;
use common::error_location::ErrorLocation;
use common::errors::{UserInputError, WalletError, WalletResult};
use common::keys::Keys;
use kaspa_bip32::Prefix;
use kaspa_consensus_core::config::params::Params;
use kaspa_grpc_client::GrpcClient;
use kaspa_wallet_core::tx::MassCalculator;
use proto::kaswallet_proto::wallet_server::WalletServer;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tonic::transport::Server;
use tracing::{debug, info, warn};

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

        let address_prefix = network_id.network_type.into();
        let address_manager = Arc::new(Mutex::new(AddressManager::new(
            keys.clone(),
            address_prefix,
        )));
        let utxo_manager = Arc::new(Mutex::new(utxo_manager::UtxoManager::new(
            address_manager.clone(),
            consensus_params.clone(),
        )));
        let subnetwork_id = resolve_subnetwork_id(self.args.subnetwork_id);
        // Warn only when (a) the env var is set, (b) the resolved id is
        // non-native (so there IS a routing redirect to surface), and
        // (c) the env-supplied value matches the resolved id (so env
        // could have been the source). This eliminates the false
        // positives the previous heuristic produced when the operator
        // passed --subnetwork-id explicitly and the env var was unset
        // or set to the same value — the warning's stated purpose is
        // "env-driven routing redirect", not "env var happens to be set".
        if !subnetwork_id.is_native() {
            if let Ok(env_val) = std::env::var("KASWALLET_SUBNETWORK_ID") {
                let env_resolves_to_same = crate::args::parse_subnetwork_id_arg(env_val.as_str())
                    .map(|env_id| env_id == subnetwork_id)
                    .unwrap_or(false);
                if env_resolves_to_same {
                    warn!(
                        "subnetwork id may be sourced from KASWALLET_SUBNETWORK_ID env var \
                         (resolved={subnetwork_id}); verify the env source is trusted"
                    );
                }
            }
        }
        // Operator explicitly opted into native by passing `00000000` (or
        // setting `KASWALLET_SUBNETWORK_ID=00000000`). `is_native()` makes
        // the lane-enforcement guard a no-op for *any* incoming tx — for
        // a deployment intended to run as an IGRA lane container, that is
        // a silent misconfiguration. Surface it loudly at startup so the
        // operator notices before the daemon serves traffic.
        if self.args.subnetwork_id.is_some() && subnetwork_id.is_native() {
            warn!(
                "--subnetwork-id resolved to native ({subnetwork_id}); lane enforcement is \
                 DISABLED for wire-supplied transactions. If this daemon is intended to run \
                 as a non-native lane (e.g. IGRA 97b10000), set --subnetwork-id (or the \
                 KASWALLET_SUBNETWORK_ID env var) to the lane's namespace."
            );
        }
        info!(
            "Transaction generator subnetwork_id: {} (native={})",
            subnetwork_id,
            subnetwork_id.is_native(),
        );
        let transaction_generator = Arc::new(Mutex::new(TransactionGenerator::new(
            kaspa_rpc_client.clone(),
            keys.clone(),
            address_manager.clone(),
            mass_calculator.clone(),
            address_prefix,
            subnetwork_id,
            &consensus_params,
        )?));
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
            subnetwork_id,
        );

        // Parse `--listen` at daemon-start time rather than inside the
        // spawned server task so a bad address surfaces as a structured
        // startup error instead of crashing an async task with a useless
        // backtrace.
        let listen: std::net::SocketAddr = self.args.listen.parse().map_err(|e| {
            WalletError::from(UserInputError::InvalidArgument {
                reason: format!(
                    "--listen must be a valid socket address (e.g. 127.0.0.1:8082): {e}"
                ),
                location: ErrorLocation::capture(),
            })
        })?;
        let listen_display = self.args.listen.clone();
        let server_handle = tokio::spawn(async move {
            info!("Starting wallet server on {}", listen_display);
            let server = WalletServer::new(service);
            if let Err(e) = Server::builder().add_service(server).serve(listen).await {
                // Log + exit the task cleanly instead of panicking. A
                // panic here unwinds an async runtime task whose stack
                // may contain in-flight secret material (passwords,
                // mnemonics) — a clean error log is safer for a wallet.
                tracing::error!(error = %e, "wallet server task exited with error");
            }
        });
        Ok((sync_manager_handle, server_handle))
    }
}
