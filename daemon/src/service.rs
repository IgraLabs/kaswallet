use crate::address_manager::AddressManager;
use crate::args::Args;
use crate::model::{Keychain, WalletAddress, WalletUtxo};
use crate::sync_manager::SyncManager;
use common::keys::Keys;
use kaspa_wallet_core::utxo::NetworkParams;
use kaspa_wrpc_client::prelude::RpcApi;
use kaspa_wrpc_client::KaspaRpcClient;
use log::{error, info, trace};
use std::collections::HashMap;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use wallet_proto::wallet_proto::wallet_server::Wallet;
use wallet_proto::wallet_proto::{
    AddressBalances, BroadcastRequest, BroadcastResponse, CreateUnsignedTransactionsRequest,
    CreateUnsignedTransactionsResponse, GetAddressesRequest, GetAddressesResponse,
    GetBalanceRequest, GetBalanceResponse, GetUtxosRequest, GetUtxosResponse, GetVersionRequest,
    GetVersionResponse, NewAddressRequest, NewAddressResponse, SendRequest, SendResponse,
    SignRequest, SignResponse,
};

#[derive(Debug)]
pub struct KasWalletService {
    args: Args,
    kaspa_rpc_client: Arc<KaspaRpcClient>,
    keys: Arc<Keys>,
    address_manager: Arc<Mutex<AddressManager>>,
    sync_manager: Arc<Mutex<SyncManager>>,
    coinbase_maturity: u64, // Is different in testnet
}

impl KasWalletService {
    pub fn new(
        args: Args,
        kaspa_rpc_client: Arc<KaspaRpcClient>,
        address_manager: Arc<Mutex<AddressManager>>,
        sync_manager: Arc<Mutex<SyncManager>>,
        keys: Arc<Keys>,
    ) -> Self {
        let network_params = NetworkParams::from(args.network());
        let coinbase_maturity = network_params
            .coinbase_transaction_maturity_period_daa
            .load(Relaxed);

        Self {
            args,
            kaspa_rpc_client,
            address_manager,
            sync_manager,
            keys,
            coinbase_maturity,
        }
    }
}

impl KasWalletService {
    async fn check_is_synced(&self) -> Result<(), Status> {
        let sync_manager = self.sync_manager.lock().await;
        if !sync_manager.is_synced().await {
            return Err(Status::failed_precondition(
                "Wallet is not synced yet. Please wait for the sync to complete.",
            ));
        }
        Ok(())
    }

    fn is_utxo_spendable(&self, utxo: WalletUtxo, virtual_daa_score: u64) -> bool {
        if !utxo.utxo_entry.is_coinbase {
            return true;
        }

        utxo.utxo_entry.block_daa_score + self.coinbase_maturity <= virtual_daa_score
    }
}

struct BalancesEntry {
    pub available: u64,
    pub pending: u64,
}

impl BalancesEntry {
    fn new() -> Self {
        Self {
            available: 0,
            pending: 0,
        }
    }

    pub fn add(&mut self, other: Self) {
        self.add_available(other.available);
        self.add_pending(other.pending);
    }
    pub fn add_available(&mut self, amount: u64) {
        self.available += amount;
    }
    pub fn add_pending(&mut self, amount: u64) {
        self.pending += amount;
    }
}

#[tonic::async_trait]
impl Wallet for KasWalletService {
    async fn get_addresses(
        &self,
        request: Request<GetAddressesRequest>,
    ) -> Result<Response<GetAddressesResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());

        self.check_is_synced().await?;

        let mut addresses = vec![];
        let address_manager = self.address_manager.lock().await;
        for i in 0..self.keys.last_used_external_index.load(Relaxed) {
            let wallet_address = WalletAddress {
                index: i,
                cosigner_index: self.keys.cosigner_index,
                keychain: Keychain::External,
            };
            match address_manager.calculate_address(&wallet_address) {
                Ok(address) => addresses.push(address.to_string()),
                Err(e) => {
                    return Err(Status::internal(format!(
                        "Failed to calculate address: {}",
                        e
                    )))
                }
            }
        }

        Ok(Response::new(GetAddressesResponse { address: addresses }))
    }

    async fn new_address(
        &self,
        request: Request<NewAddressRequest>,
    ) -> Result<Response<NewAddressResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());

        self.check_is_synced().await?;

        let address_manager = self.address_manager.lock().await;

        match address_manager.new_address().await {
            Ok((address, _)) => {
                let response = NewAddressResponse { address };
                Ok(Response::new(response))
            }
            Err(e) => Err(Status::internal(format!(
                "Failed to generate new address: {}",
                e
            ))),
        }
    }

    async fn get_balance(
        &self,
        request: Request<GetBalanceRequest>,
    ) -> Result<Response<GetBalanceResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());

        self.check_is_synced().await?;

        let block_dag_info = match self.kaspa_rpc_client.get_block_dag_info().await {
            Ok(block_dag_info) => block_dag_info,
            Err(e) => {
                error!("Failed to get block DAG info: {}", e);
                return Err(Status::internal("Internal server error"));
            }
        };

        let virtual_daa_score = block_dag_info.virtual_daa_score;
        let mut balances_map = HashMap::new();

        let utxos_sorted_by_amount: Vec<WalletUtxo>;
        {
            let sync_manager = self.sync_manager.lock().await;
            utxos_sorted_by_amount = sync_manager.get_utxos_sorted_by_amount().await;
        }

        let utxos_count = utxos_sorted_by_amount.len();
        for entry in utxos_sorted_by_amount {
            let amount = entry.utxo_entry.amount;
            let address = entry.address.clone();
            let balances = balances_map
                .entry(address.clone())
                .or_insert_with(BalancesEntry::new);
            if self.is_utxo_spendable(entry, virtual_daa_score) {
                balances.add_available(amount);
            } else {
                balances.add_pending(amount);
            }
        }
        let mut address_balances = vec![];
        let mut total_balances = BalancesEntry::new();

        let address_manager = self.address_manager.lock().await;
        for (wallet_address, balances) in balances_map {
            let address = match address_manager.calculate_address(&wallet_address) {
                Ok(address) => address,
                Err(e) => {
                    error!("Failed to calculate address: {}", e);
                    return Err(Status::internal("Internal server error"));
                }
            };
            address_balances.push(AddressBalances {
                address: address.to_string(),
                available: balances.available,
                pending: balances.pending,
            });
            total_balances.add(balances);
        }

        info!(
            "GetBalance request scanned {} UTXOs overll over {} addresses",
            utxos_count,
            address_balances.len()
        );

        Ok(Response::new(GetBalanceResponse {
            available: total_balances.available,
            pending: total_balances.pending,
            address_balances,
        }))
    }

    async fn get_utxos(
        &self,
        request: Request<GetUtxosRequest>,
    ) -> Result<Response<GetUtxosResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());
        todo!()
    }

    async fn create_unsigned_transactions(
        &self,
        request: Request<CreateUnsignedTransactionsRequest>,
    ) -> Result<Response<CreateUnsignedTransactionsResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());
        todo!()
    }

    async fn sign(&self, request: Request<SignRequest>) -> Result<Response<SignResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());
        todo!()
    }

    async fn broadcast(
        &self,
        request: Request<BroadcastRequest>,
    ) -> Result<Response<BroadcastResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());
        todo!()
    }

    async fn send(&self, request: Request<SendRequest>) -> Result<Response<SendResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());
        todo!()
    }

    async fn get_version(
        &self,
        request: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref().to_owned());

        Ok(Response::new(GetVersionResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }))
    }
}
