use crate::address_manager::{AddressManager, AddressSet};
use crate::args::Args;
use crate::model::{Keychain, UserInputError, WalletAddress, WalletUtxo};
use crate::sync_manager::SyncManager;
use common::keys::Keys;
use kaspa_addresses::Address;
use kaspa_p2p_lib::pb::TransactionMessage;
use kaspa_wallet_core::utxo::NetworkParams;
use kaspa_wrpc_client::prelude::RpcApi;
use kaspa_wrpc_client::KaspaRpcClient;
use log::{error, info, trace};
use prost::Message;
use std::collections::HashMap;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use wallet_proto::wallet_proto::wallet_server::Wallet;
use wallet_proto::wallet_proto::{
    AddressBalances, AddressToUtxos, BroadcastRequest, BroadcastResponse,
    CreateUnsignedTransactionsRequest, CreateUnsignedTransactionsResponse, GetAddressesRequest,
    GetAddressesResponse, GetBalanceRequest, GetBalanceResponse, GetUtxosRequest, GetUtxosResponse,
    GetVersionRequest, GetVersionResponse, NewAddressRequest, NewAddressResponse, SendRequest,
    SendResponse, SignRequest, SignResponse, Utxo as ProtoUtxo,
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
    async fn filter_utxos_and_bucket_by_address(
        &self,
        utxos: &Vec<WalletUtxo>,
        fee_rate: f64,
        virtual_daa_score: u64,
        addresses: Vec<String>,
        include_pending: bool,
        include_dust: bool,
    ) -> HashMap<String, Vec<ProtoUtxo>> {
        let mut filtered_bucketed_utxos = HashMap::new();
        let address_manager = self.address_manager.lock().await;
        for utxo in utxos {
            let is_pending = self.is_utxo_pending(utxo, virtual_daa_score);
            if !include_pending && is_pending {
                continue;
            }
            let is_dust = self.is_utxo_dust(utxo, fee_rate);
            if !include_dust && is_dust {
                continue;
            }

            // TODO: Don't calculate address every time
            let address = address_manager
                .calculate_address(&utxo.address)
                .unwrap()
                .address_to_string();

            if !addresses.is_empty() && !addresses.contains(&address) {
                continue;
            }

            let entry = filtered_bucketed_utxos
                .entry(address)
                .or_insert_with(Vec::new);
            entry.push(utxo.to_owned().into_proto(is_pending, is_dust));
        }

        filtered_bucketed_utxos
    }

    async fn get_virtual_daa_score(&self) -> Result<u64, Status> {
        let block_dag_info = match self.kaspa_rpc_client.get_block_dag_info().await {
            Ok(block_dag_info) => block_dag_info,
            Err(e) => {
                error!("Failed to get block DAG info: {}", e);
                return Err(Status::internal("Internal server error"));
            }
        };
        let virtual_daa_score = block_dag_info.virtual_daa_score;

        Ok(virtual_daa_score)
    }
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

    fn is_utxo_dust(&self, utxo: &WalletUtxo, fee_rate: f64) -> bool {
        todo!()
    }

    fn is_utxo_pending(&self, utxo: &WalletUtxo, virtual_daa_score: u64) -> bool {
        if !utxo.utxo_entry.is_coinbase {
            return false;
        }

        utxo.utxo_entry.block_daa_score + self.coinbase_maturity > virtual_daa_score
    }
}

#[derive(Clone)]
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
        trace!("Received request: {:?}", request.get_ref());

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
        trace!("Received request: {:?}", request.get_ref());

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
        trace!("Received request: {:?}", request.get_ref());

        self.check_is_synced().await?;

        let virtual_daa_score = self.get_virtual_daa_score().await?;
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
            if self.is_utxo_pending(&entry, virtual_daa_score) {
                balances.add_pending(amount);
            } else {
                balances.add_available(amount);
            }
        }
        let mut address_balances = vec![];
        let mut total_balances = BalancesEntry::new();

        let address_manager = self.address_manager.lock().await;
        let include_balance_per_address = request.get_ref().include_balance_per_address;
        for (wallet_address, balances) in balances_map.clone() {
            let address = match address_manager.calculate_address(&wallet_address) {
                Ok(address) => address,
                Err(e) => {
                    error!("Failed to calculate address: {}", e);
                    return Err(Status::internal("Internal server error"));
                }
            };
            if include_balance_per_address {
                address_balances.push(AddressBalances {
                    address: address.to_string(),
                    available: balances.available,
                    pending: balances.pending,
                });
            }
            total_balances.add(balances);
        }

        info!(
            "GetBalance request scanned {} UTXOs overall over {} addresses",
            utxos_count,
            balances_map.len()
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
        trace!("Received request: {:?}", request.get_ref());

        let request = request.get_ref();
        let mut addresses = request.addresses.clone();
        for address in &addresses {
            if let Err(e) = Address::try_from(address.as_str()) {
                return Err(Status::invalid_argument(format!(
                    "Address {} is invalid: {}",
                    address, e
                )));
            }
        }

        let address_set: AddressSet;
        {
            let address_manager = self.address_manager.lock().await;
            address_set = address_manager.address_set().await;
        }
        if addresses.len() == 0 {
            addresses = address_set.keys().cloned().collect();
        } else {
            for address in &addresses {
                if !address_set.contains_key(address) {
                    return Err(Status::invalid_argument(format!(
                        "Address {} not found in wallet",
                        address
                    )));
                }
            }
        }

        let fee_estimate = match self.kaspa_rpc_client.get_fee_estimate().await {
            Ok(fee_estimate) => fee_estimate,
            Err(e) => {
                error!("Failed to get fee estimate from RPC: {}", e);
                return Err(Status::internal("Internal server error"));
            }
        };

        let fee_rate = fee_estimate.normal_buckets[0].feerate;

        let virtual_daa_score = self.get_virtual_daa_score().await?;

        let utxos: Vec<WalletUtxo>;
        {
            let sync_manager = self.sync_manager.lock().await;
            utxos = sync_manager.get_utxos_sorted_by_amount().await;
        }

        let filtered_bucketed_utxos = self
            .filter_utxos_and_bucket_by_address(
                &utxos,
                fee_rate,
                virtual_daa_score,
                addresses,
                request.include_pending,
                request.include_dust,
            )
            .await;

        let addresses_to_utxos = filtered_bucketed_utxos
            .iter()
            .map(|(address_string, utxos)| AddressToUtxos {
                address: address_string.to_string(),
                utxos: utxos.clone(),
            })
            .collect();
        Ok(Response::new(GetUtxosResponse { addresses_to_utxos }))
    }

    async fn create_unsigned_transactions(
        &self,
        request: Request<CreateUnsignedTransactionsRequest>,
    ) -> Result<Response<CreateUnsignedTransactionsResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref());
        let request = request.into_inner();
        let transaction_description = match request.transaction_description {
            Some(description) => description,
            None => {
                return Err(Status::invalid_argument(
                    "Transaction description is required",
                ))
            }
        };

        // TODO: implement manual utxo selection
        if !transaction_description.utxos.is_empty() {
            return Err(Status::invalid_argument("UTXOs are not supported yet"));
        }

        self.check_is_synced().await?;

        let sync_manager = self.sync_manager.lock().await;

        let unsigned_transactions_result = sync_manager
            .create_unsigned_transactions(
                transaction_description.to_address,
                transaction_description.amount,
                transaction_description.is_send_all,
                transaction_description.payload,
                transaction_description.from_addresses,
                transaction_description.utxos,
                transaction_description.use_existing_change_address,
                transaction_description.fee_policy,
            )
            .await;
        let unsigned_transactions = match unsigned_transactions_result {
            Ok(unsigned_transactions) => unsigned_transactions,
            Err(e) => {
                return match e.downcast::<UserInputError>() {
                    Ok(e) => Err(Status::invalid_argument(e.message)),
                    Err(_) => Err(Status::internal("Internal server error")),
                }
            }
        };

        let encoded_unsigned_transactions = unsigned_transactions
            .iter()
            .map(|unsigned_transaction| {
                let transaction_message = TransactionMessage::from(unsigned_transaction);
                transaction_message.encode_to_vec()
            })
            .collect();

        Ok(Response::new(CreateUnsignedTransactionsResponse {
            unsigned_transactions: encoded_unsigned_transactions,
        }))
    }

    async fn sign(&self, request: Request<SignRequest>) -> Result<Response<SignResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref());
        todo!()
    }

    async fn broadcast(
        &self,
        request: Request<BroadcastRequest>,
    ) -> Result<Response<BroadcastResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref());
        todo!()
    }

    async fn send(&self, request: Request<SendRequest>) -> Result<Response<SendResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref());
        todo!()
    }

    async fn get_version(
        &self,
        request: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref());

        Ok(Response::new(GetVersionResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }))
    }
}
