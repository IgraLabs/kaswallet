use crate::address_manager::{AddressManager, AddressSet};
use crate::model::{Keychain, WalletAddress, WalletSignableTransaction, WalletUtxo};
use crate::sync_manager::SyncManager;
use crate::transaction_generator::TransactionGenerator;
use crate::utxo_manager::UtxoManager;
use common::errors::WalletError;
use common::keys::Keys;
use itertools::Itertools;
use kaspa_addresses::Address;
use kaspa_bip32::{secp256k1, DerivationPath, ExtendedPrivateKey, Mnemonic, SecretKey};
use kaspa_consensus_core::hashing::sighash::{
    calc_schnorr_signature_hash, SigHashReusedValuesUnsync,
};
use kaspa_consensus_core::hashing::sighash_type::SIG_HASH_ALL;
use kaspa_consensus_core::sign::Signed::{Fully, Partially};
use kaspa_consensus_core::sign::{verify, Signed};
use kaspa_consensus_core::tx::SignableTransaction;
use kaspa_wallet_core::rpc::RpcApi;
use kaspa_wrpc_client::KaspaRpcClient;
use kaswallet_proto::kaswallet_proto::wallet_server::Wallet;
use kaswallet_proto::kaswallet_proto::{
    AddressBalances, AddressToUtxos, BroadcastRequest, BroadcastResponse,
    CreateUnsignedTransactionsRequest, CreateUnsignedTransactionsResponse, GetAddressesRequest,
    GetAddressesResponse, GetBalanceRequest, GetBalanceResponse, GetUtxosRequest, GetUtxosResponse,
    GetVersionRequest, GetVersionResponse, NewAddressRequest, NewAddressResponse, SendRequest,
    SendResponse, SignRequest, SignResponse, TransactionDescription, Utxo as ProtoUtxo,
};
use log::{debug, error, info, trace};
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::iter::once;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};

pub struct KasWalletService {
    kaspa_rpc_client: Arc<KaspaRpcClient>,
    keys: Arc<Keys>,
    address_manager: Arc<Mutex<AddressManager>>,
    utxo_manager: Arc<Mutex<UtxoManager>>,
    transaction_generator: Arc<Mutex<TransactionGenerator>>,
    sync_manager: Arc<Mutex<SyncManager>>,
}

impl KasWalletService {
    pub fn new(
        kaspa_rpc_client: Arc<KaspaRpcClient>,
        keys: Arc<Keys>,
        address_manager: Arc<Mutex<AddressManager>>,
        utxo_manager: Arc<Mutex<UtxoManager>>,
        transaction_generator: Arc<Mutex<TransactionGenerator>>,
        sync_manager: Arc<Mutex<SyncManager>>,
    ) -> Self {
        Self {
            kaspa_rpc_client,
            keys,
            address_manager,
            utxo_manager,
            transaction_generator,
            sync_manager,
        }
    }
    async fn check_is_synced(&self) -> Result<(), Status> {
        let sync_manager = self.sync_manager.lock().await;
        if !sync_manager.is_synced().await {
            return Err(Status::failed_precondition(
                "Wallet is not synced yet. Please wait for the sync to complete.",
            ));
        }
        Ok(())
    }

    fn is_utxo_dust(&self, _utxo: &WalletUtxo, _fee_rate: f64) -> bool {
        // TODO: actually calculate if utxo is dust
        false
    }
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
                let utxo_manager = self.utxo_manager.lock().await;
                is_pending = utxo_manager.is_utxo_pending(utxo, virtual_daa_score);
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
                address = address_manager
                    .kaspa_address_from_wallet_address(&utxo.address, true)
                    .await
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
        transactions: &Vec<WalletSignableTransaction>,
    ) -> Result<Vec<Vec<u8>>, Status> {
        let mut encoded_transactions = vec![];
        for unsigned_transaction in transactions {
            // TODO: Use protobuf instead of borsh for serialization
            let encoded_transaction = borsh::to_vec(&unsigned_transaction)?;
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
                .map_err(|e| {
                    Status::invalid_argument(format!("Unable to decode transactions: {}", e))
                })?;
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
            Status::invalid_argument("Failed to decrypt mnemonics (probably an invalid password?)")
        })?;
        let extended_private_keys = Self::mnemonics_to_private_keys(&mnemonics)?;

        let mut signed_transactions = vec![];
        for unsigned_transaction in unsigned_transactions {
            let derivation_paths = unsigned_transaction.derivation_paths.clone();
            let address_by_input_index = unsigned_transaction.address_by_input_index.clone();

            let signed_transaction = self
                .sign_transaction(unsigned_transaction, &extended_private_keys)
                .map_err(|e| {
                    Status::invalid_argument(format!("Failed to sign transaction: {}", e))
                })?;
            let wallet_signed_transaction = WalletSignableTransaction::new(
                signed_transaction,
                derivation_paths,
                address_by_input_index,
            );

            signed_transactions.push(wallet_signed_transaction);
        }

        Ok(signed_transactions)
    }

    fn sign_transaction(
        &self,
        unsigned_transaction: WalletSignableTransaction,
        extended_private_keys: &Vec<ExtendedPrivateKey<SecretKey>>,
    ) -> Result<Signed, Box<dyn Error + Send + Sync>> {
        let mut private_keys = vec![];
        for derivation_path in &unsigned_transaction.derivation_paths {
            for extended_private_key in extended_private_keys.iter() {
                let private_key = extended_private_key.clone().derive_path(derivation_path)?;
                private_keys.push(private_key.private_key().secret_bytes());
            }
        }

        let signable_transaction = unsigned_transaction.transaction;
        let signed_transaction = sign_with_multiple(signable_transaction.unwrap(), &private_keys);

        sanity_check_verify(&signed_transaction)?;
        Ok(signed_transaction)
    }

    fn mnemonics_to_private_keys(
        mnemonics: &Vec<Mnemonic>,
    ) -> Result<Vec<ExtendedPrivateKey<SecretKey>>, Status> {
        let mut private_keys = vec![];
        for mnemonic in mnemonics {
            let seed = mnemonic.to_seed("");
            let x_private_key = ExtendedPrivateKey::new(seed).map_err(|e| {
                error!("Failed to create extended private key: {}", e);
                Status::internal("Internal server error")
            })?;
            let master_key_derivation_path = master_key_path(mnemonics.len() > 1);
            let private_key = x_private_key
                .derive_path(&master_key_derivation_path)
                .unwrap();

            private_keys.push(private_key)
        }
        Ok(private_keys)
    }

    async fn submit_transactions(
        &self,
        signed_transactions: &Vec<WalletSignableTransaction>,
    ) -> Result<Vec<String>, Status> {
        let mut transaction_ids = vec![];
        for signed_transaction in signed_transactions {
            if let Partially(_) = signed_transaction.transaction {
                return Err(Status::invalid_argument("Transaction is not fully signed"));
            }

            let tx = match &signed_transaction.transaction {
                Fully(tx) => tx,
                Partially(tx) => tx,
            };
            let rpc_transaction = (&tx.tx).into();
            let submit_result = self
                .kaspa_rpc_client
                .submit_transaction(rpc_transaction, false)
                .await;

            match submit_result {
                Err(e) => {
                    return Err(Status::invalid_argument(format!(
                        "Failed to submit transaction: {}",
                        e
                    )));
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

    async fn create_unsigned_transactions(
        &self,
        transaction_description: TransactionDescription,
    ) -> Result<Vec<WalletSignableTransaction>, Status> {
        // TODO: implement manual utxo selection
        if !transaction_description.utxos.is_empty() {
            return Err(Status::invalid_argument("UTXOs are not supported yet"));
        }

        self.check_is_synced().await?;

        let unsigned_transactions_result: Result<
            Vec<WalletSignableTransaction>,
            Box<dyn Error + Send + Sync>,
        >;
        {
            let mut transaction_generator = self.transaction_generator.lock().await;
            unsigned_transactions_result = transaction_generator
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
        }
        let unsigned_transactions = match unsigned_transactions_result {
            Ok(unsigned_transactions) => unsigned_transactions,
            Err(e) => {
                return match e.downcast::<WalletError>() {
                    Ok(e) => match e.as_ref() {
                        WalletError::SanityCheckFailed(e) => {
                            error!("Sanity check failed: {}", e);
                            internal_server_error()
                        }
                        WalletError::UserInputError(e) => {
                            debug!("User input error: {}", e);
                            Err(Status::invalid_argument(e))
                        }
                    },
                    Err(e) => {
                        error!("Error creating unsigned transaction: {}", e);
                        internal_server_error()
                    }
                };
            }
        };
        Ok(unsigned_transactions)
    }
}

fn internal_server_error<T>() -> Result<T, Status> {
    Err(Status::internal("Internal server error"))
}

fn sanity_check_verify(signed_transaction: &Signed) -> Result<(), Status> {
    if let Fully(_) = signed_transaction {
        debug!("Transaction is fully signed");
    }
    if let Partially(_) = signed_transaction {
        debug!("Transaction is partially signed, so can't verify");
        return Ok(());
    }
    let verifiable_transaction = &signed_transaction.unwrap_ref().as_verifiable();
    let verify_result = verify(verifiable_transaction);
    if let Err(e) = verify_result {
        error!("Signed transaction does not verify correctly: {}", e);
        Err(Status::internal("Internal server error"))
    } else {
        debug!("Signed transaction verifies correctly");
        Ok(())
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
        for i in 1..=self.keys.last_used_external_index.load(Relaxed) {
            let wallet_address = WalletAddress {
                index: i,
                cosigner_index: self.keys.cosigner_index,
                keychain: Keychain::External,
            };
            match address_manager
                .kaspa_address_from_wallet_address(&wallet_address, true)
                .await
            {
                Ok(address) => {
                    addresses.push(address.to_string());
                }
                Err(e) => {
                    return Err(Status::internal(format!(
                        "Failed to calculate address: {}",
                        e
                    )));
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

        let utxos_sorted_by_amount: &Vec<WalletUtxo>;
        let utxos_count: usize;
        {
            let utxo_manager = self.utxo_manager.lock().await;
            utxos_sorted_by_amount = utxo_manager.utxos_sorted_by_amount();

            utxos_count = utxos_sorted_by_amount.len();
            for entry in utxos_sorted_by_amount {
                let amount = entry.utxo_entry.amount;
                let address = entry.address.clone();
                let balances = balances_map
                    .entry(address.clone())
                    .or_insert_with(BalancesEntry::new);
                if utxo_manager.is_utxo_pending(&entry, virtual_daa_score) {
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
            let address = match address_manager
                .kaspa_address_from_wallet_address(&wallet_address, true)
                .await
            {
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

        let filtered_bucketed_utxos: HashMap<String, Vec<ProtoUtxo>>;
        {
            let utxo_manager = self.utxo_manager.lock().await;
            let utxos = utxo_manager.utxos_sorted_by_amount();

            filtered_bucketed_utxos = self
                .filter_utxos_and_bucket_by_address(
                    utxos,
                    fee_rate,
                    virtual_daa_score,
                    addresses,
                    request.include_pending,
                    request.include_dust,
                )
                .await;
        }

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
                ));
            }
        };

        let unsigned_transactions = self
            .create_unsigned_transactions(transaction_description)
            .await?;

        Ok(Response::new(CreateUnsignedTransactionsResponse {
            unsigned_transactions: Self::encode_transactions(&unsigned_transactions)?,
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

        let encoded_signed_transactions = Self::encode_transactions(&signed_transactions)?;

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
        let signed_transactions = Self::decode_transactions(&encoded_signed_transactions)?;

        let transaction_ids = self.submit_transactions(&signed_transactions).await?;

        Ok(Response::new(BroadcastResponse { transaction_ids }))
    }

    async fn send(&self, request: Request<SendRequest>) -> Result<Response<SendResponse>, Status> {
        trace!("Received request: {:?}", request.get_ref());

        let request = request.into_inner();
        let transaction_description = match request.transaction_description {
            Some(description) => description,
            None => {
                return Err(Status::invalid_argument(
                    "Transaction description is required",
                ));
            }
        };

        let unsigned_transactions = self
            .create_unsigned_transactions(transaction_description)
            .await?;

        let signed_transactions = self
            .sign_transactions(unsigned_transactions, &request.password)
            .await?;

        let submit_transactions_result = self.submit_transactions(&signed_transactions).await;
        if let Err(e) = submit_transactions_result {
            error!("Failed to submit transactions: {}", e);
            return Err(e);
        }
        let transaction_ids = submit_transactions_result?;
        let encoded_signed_transactions = Self::encode_transactions(&signed_transactions)?;

        Ok(Response::new(SendResponse {
            transaction_ids,
            signed_transactions: encoded_signed_transactions,
        }))
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

// This is a copy of the sign_with_multiple_v2 function from the wallet core
// With the following addition: Update the sig_op_count
pub fn sign_with_multiple(mut mutable_tx: SignableTransaction, privkeys: &[[u8; 32]]) -> Signed {
    let mut map = BTreeMap::new();
    for privkey in privkeys {
        let schnorr_key =
            secp256k1::Keypair::from_seckey_slice(secp256k1::SECP256K1, privkey).unwrap();
        let schnorr_public_key = schnorr_key.public_key().x_only_public_key().0;
        let script_pub_key_script = once(0x20)
            .chain(schnorr_public_key.serialize().into_iter())
            .chain(once(0xac))
            .collect_vec();
        map.insert(script_pub_key_script, schnorr_key);
    }

    let reused_values = SigHashReusedValuesUnsync::new();
    let mut additional_signatures_required = false;
    for i in 0..mutable_tx.tx.inputs.len() {
        let script = mutable_tx.entries[i]
            .as_ref()
            .unwrap()
            .script_public_key
            .script();
        if let Some(schnorr_key) = map.get(script) {
            let sig_hash = calc_schnorr_signature_hash(
                &mutable_tx.as_verifiable(),
                i,
                SIG_HASH_ALL,
                &reused_values,
            );
            let msg =
                secp256k1::Message::from_digest_slice(sig_hash.as_bytes().as_slice()).unwrap();
            let sig: [u8; 64] = *schnorr_key.sign_schnorr(msg).as_ref();
            // This represents OP_DATA_65 <SIGNATURE+SIGHASH_TYPE> (since signature length is 64 bytes and SIGHASH_TYPE is one byte)
            mutable_tx.tx.inputs[i].signature_script = once(65u8)
                .chain(sig)
                .chain([SIG_HASH_ALL.to_u8()])
                .collect();
        } else {
            additional_signatures_required = true;
        }
    }
    if additional_signatures_required {
        Partially(mutable_tx)
    } else {
        Fully(mutable_tx)
    }
}

// TODO: combine with the function in create
const SINGLE_SINGER_PURPOSE: u32 = 44;
const MULTISIG_PURPOSE: u32 = 45;
const KASPA_COIN_TYPE: u32 = 111111;
fn master_key_path(is_multisig: bool) -> DerivationPath {
    let purpose = if is_multisig {
        MULTISIG_PURPOSE
    } else {
        SINGLE_SINGER_PURPOSE
    };
    let path_string = format!("m/{}'/{}'/0'", purpose, KASPA_COIN_TYPE);
    path_string.parse().unwrap()
}
