use crate::address_manager::{AddressManager, AddressSet};
use crate::args::Args;
use crate::model::{
    Keychain, UserInputError, WalletAddress, WalletSignableTransaction, WalletUtxo,
};
use crate::sync_manager::SyncManager;
use common::keys::Keys;
use kaspa_addresses::Address;
use kaspa_bip32::{ChildNumber, ExtendedPrivateKey, Mnemonic, PrivateKey, SecretKey};
use kaspa_consensus_core::sign::Signed::Partially;
use kaspa_consensus_core::sign::{sign_with_multiple, sign_with_multiple_v2, Signed};
use kaspa_consensus_core::tx::SignableTransaction;
use kaspa_p2p_lib::pb::TransactionMessage;
use kaspa_wallet_keys::derivation::gen1::WalletDerivationManager;
use kaspa_wallet_keys::derivation_path;
use kaspa_wrpc_client::prelude::{RpcApi, RpcResult};
use kaspa_wrpc_client::KaspaRpcClient;
use log::{error, info, trace};
use prost::Message;
use std::collections::HashMap;
use std::error::Error;
use std::future::Future;
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
        for utxo in utxos {
            let is_pending: bool;
            {
                let sync_manager = self.sync_manager.lock().await;
                is_pending = sync_manager.is_utxo_pending(utxo, virtual_daa_score);
            }
            if !include_pending && is_pending {
                continue;
            }
            let is_dust = self.is_utxo_dust(utxo, fee_rate);
            if !include_dust && is_dust {
                continue;
            }

            let address: String;
            {
                let address_manager = self.address_manager.lock().await;
                // TODO: Don't calculate address every time
                address = address_manager
                    .calculate_address(&utxo.address)
                    .unwrap()
                    .address_to_string();
            }

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

    fn encode_transactions(
        transactions: Vec<WalletSignableTransaction>,
    ) -> Result<Vec<Vec<u8>>, Status> {
        let mut encoded_transactions = vec![];
        for unsigned_transaction in transactions {
            // TODO: Use protobuf instead of borsh for serialization
            let encoded_transaction = borsh::to_vec(&unsigned_transaction).map_err(|e| {
                error!("Failed to encode transaction: {}", e);
                Status::internal("Internal server error")
            })?;
            encoded_transactions.push(encoded_transaction);
        }
        Ok(encoded_transactions)
    }

    fn decode_transactions(
        encoded_transactions: &Vec<Vec<u8>>,
    ) -> Result<Vec<WalletSignableTransaction>, Status> {
        let mut unsigned_transactions = vec![];
        for encoded_transaction_transaction in encoded_transactions {
            let unsigned_transaction = borsh::from_slice(&encoded_transaction_transaction)
                .map_err(|e| Status::invalid_argument("Unable to decode transactions"))?;
            unsigned_transactions.push(unsigned_transaction);
        }
        Ok(unsigned_transactions)
    }
    async fn sign_transactions(
        &self,
        unsigned_transactions: Vec<WalletSignableTransaction>,
        password: &String,
    ) -> Result<Vec<WalletSignableTransaction>, Status> {
        let mnemonics = self.keys.decrypt_mnemonics(password).map_err(|e| {
            error!("Failed to decrypt mnemonics: {}", e);
            Status::internal("Invalid password")
        })?;
        let extended_private_keys = Self::mnemonics_to_private_keys(mnemonics)?;

        let mut signed_transactions = vec![];
        for unsigned_transaction in &unsigned_transactions {
            let signed_transaction = self
                .sign_transaction(unsigned_transaction, &extended_private_keys)
                .map_err(|e| {
                    Status::invalid_argument(format!("Failed to sign transaction: {}", e))
                })?;
            let wallet_signed_transaction = WalletSignableTransaction::new(
                signed_transaction,
                unsigned_transaction.derivation_paths.clone(),
            );
            signed_transactions.push(wallet_signed_transaction);
        }

        Ok(signed_transactions)
    }

    fn sign_transaction(
        &self,
        unsigned_transaction: &WalletSignableTransaction,
        extended_private_keys: &Vec<ExtendedPrivateKey<SecretKey>>,
    ) -> Result<Signed, Box<dyn Error + Send + Sync>> {
        let mut private_keys = vec![];
        for derivation_path in &unsigned_transaction.derivation_paths {
            for extended_private_key in extended_private_keys.iter() {
                let private_key = extended_private_key.clone().derive_path(derivation_path)?;
                private_keys.push(private_key.private_key().secret_bytes());
            }
        }

        let mut signable_transaction = &unsigned_transaction.transaction;
        Ok(sign_with_multiple_v2(
            signable_transaction.clone().unwrap(),
            &private_keys,
        ))
    }

    fn mnemonics_to_private_keys(
        mnemonics: Vec<Mnemonic>,
    ) -> Result<Vec<ExtendedPrivateKey<SecretKey>>, Status> {
        let mut extended_private_keys = vec![];
        for mnemonic in mnemonics {
            let seed = mnemonic.to_seed("");
            let x_private_key = ExtendedPrivateKey::new(seed).map_err(|e| {
                error!("Failed to create extended private key: {}", e);
                Status::internal("Internal server error")
            })?;

            extended_private_keys.push(x_private_key)
        }
        Ok(extended_private_keys)
    }

    async fn submit_transactions(
        &self,
        signed_transactions: Vec<WalletSignableTransaction>,
    ) -> Result<Vec<String>, Status> {
        let mut transaction_ids = vec![];
        for signed_transaction in signed_transactions {
            if let Partially(_) = signed_transaction.transaction {
                return Err(Status::invalid_argument("Transaction is not fully signed"));
            }

            let rpc_transaction = (&signed_transaction.transaction.unwrap().tx).into();
            let submit_result = self
                .kaspa_rpc_client
                .submit_transaction(rpc_transaction, false)
                .await;

            match submit_result {
                Err(e) => {
                    return Err(Status::invalid_argument(format!(
                        "Failed to submit transaction: {}",
                        e
                    )))
                }
                Ok(rpc_transaction_id) => {
                    transaction_ids.push(rpc_transaction_id.to_string());
                }
            }
        }

        let mut sync_manager = self.sync_manager.lock().await;
        sync_manager.force_sync().await.unwrap(); // unwrap is safe - force sync fails only if it wasn't initialized

        Ok(transaction_ids)
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
        Self {
            args,
            kaspa_rpc_client,
            address_manager,
            sync_manager,
            keys,
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
        // TODO: actually calculate if utxo is dust
        false
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
        let utxos_count: usize;
        {
            let sync_manager = self.sync_manager.lock().await;
            utxos_sorted_by_amount = sync_manager.get_utxos_sorted_by_amount().await;

            utxos_count = utxos_sorted_by_amount.len();
            for entry in utxos_sorted_by_amount {
                let amount = entry.utxo_entry.amount;
                let address = entry.address.clone();
                let balances = balances_map
                    .entry(address.clone())
                    .or_insert_with(BalancesEntry::new);
                if sync_manager.is_utxo_pending(&entry, virtual_daa_score) {
                    balances.add_pending(amount);
                } else {
                    balances.add_available(amount);
                }
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

        Ok(Response::new(CreateUnsignedTransactionsResponse {
            unsigned_transactions: Self::encode_transactions(unsigned_transactions)?,
        }))
    }

    async fn sign(&self, request: Request<SignRequest>) -> Result<Response<SignResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref());

        let request = request.into_inner();
        let encoded_unsigned_transactions = &request.unsigned_transactions;
        let unsigned_transactions = Self::decode_transactions(encoded_unsigned_transactions)?;

        let signed_transactions = self
            .sign_transactions(unsigned_transactions, &request.password)
            .await?;

        let encoded_signed_transactions = Self::encode_transactions(signed_transactions)?;

        Ok(Response::new(SignResponse {
            signed_transactions: encoded_signed_transactions,
        }))
    }

    async fn broadcast(
        &self,
        request: Request<BroadcastRequest>,
    ) -> Result<Response<BroadcastResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref());

        let request = request.into_inner();
        let encoded_signed_transactions = &request.transactions;
        let signed_transactions = Self::decode_transactions(encoded_signed_transactions)?;

        let transaction_ids = self.submit_transactions(signed_transactions).await?;

        Ok(Response::new(BroadcastResponse { transaction_ids }))
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
