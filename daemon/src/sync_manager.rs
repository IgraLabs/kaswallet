use crate::address_manager::AddressManager;
use crate::model::{
    UserInputError, WalletAddress, WalletOutpoint, WalletPayment, WalletSignableTransaction,
    WalletUtxo, WalletUtxoEntry,
};
use chrono::{DateTime, Duration, Utc};
use common::keys::Keys;
use kaspa_addresses::{Address, Prefix, Version};
use kaspa_bip32::{secp256k1, ExtendedPrivateKey, PublicKey, PublicKeyBytes, SecretKey, KEY_SIZE};
use kaspa_consensus_core::constants::SOMPI_PER_KASPA;
use kaspa_consensus_core::network::NetworkId;
use kaspa_consensus_core::tx::{
    SignableTransaction, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput,
    UtxoEntry,
};
use kaspa_txscript::pay_to_address_script;
use kaspa_wallet_core::prelude::AddressPrefix;
use kaspa_wallet_core::tx::MassCalculator;
use kaspa_wallet_core::utxo::NetworkParams;
use kaspa_wrpc_client::prelude::{
    RpcAddress, RpcApi, RpcMempoolEntryByAddress, RpcUtxosByAddressesEntry,
};
use kaspa_wrpc_client::KaspaRpcClient;
use log::{debug, error, info};
use std::cmp::min;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::ops::Add;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio::time::interval;
use wallet_proto::wallet_proto::{fee_policy, FeePolicy, Outpoint};

const SYNC_INTERVAL: u64 = 10; // seconds

// The minimal change amount to target in order to avoid large storage mass (see KIP9 for more details).
// By having at least 10KAS in the change output we make sure that the storage mass charged for change is
// at most 1000 gram. Generally, if the payment is above 10KAS as well, the resulting storage mass will be
// in the order of magnitude of compute mass and wil not incur additional charges.
// Additionally, every transaction with send value > ~0.1 KAS should succeed (at most ~99K storage mass for payment
// output, thus overall lower than standard mass upper bound which is 100K gram)
const MIN_CHANGE_TARGET: u64 = SOMPI_PER_KASPA * 10;

// The current minimal fee rate according to mempool standards
const MIN_FEE_RATE: f64 = 1.0;

pub struct SyncManager {
    kaspa_rpc_client: Arc<KaspaRpcClient>,
    address_manager: Arc<Mutex<AddressManager>>,
    mass_calculator: MassCalculator,
    keys: Arc<Keys>,
    address_prefix: AddressPrefix,

    start_time_of_last_completed_refresh: Mutex<DateTime<Utc>>,
    utxos_sorted_by_amount: Mutex<Vec<WalletUtxo>>,
    mempool_excluded_utxos: Mutex<HashMap<WalletOutpoint, WalletUtxo>>,
    used_outpoints: Mutex<HashMap<WalletOutpoint, DateTime<Utc>>>,
    first_sync_done: AtomicBool,

    coinbase_maturity: u64, // Is different in testnet

    force_sync_sender: Option<mpsc::Sender<()>>,
}

impl SyncManager {
    pub fn new(
        network_id: NetworkId,
        kaspa_rpc_client: Arc<KaspaRpcClient>,
        address_manager: Arc<Mutex<AddressManager>>,
        keys: Arc<Keys>,
        address_prefix: Prefix,
    ) -> Self {
        let network_params = NetworkParams::from(network_id);
        let coinbase_maturity = network_params
            .coinbase_transaction_maturity_period_daa
            .load(Relaxed);
        let mass_calculator = MassCalculator::new(&network_id.network_type.into());

        Self {
            kaspa_rpc_client,
            address_manager,
            mass_calculator,
            keys,
            address_prefix,
            start_time_of_last_completed_refresh: Mutex::new(DateTime::<Utc>::MIN_UTC),
            utxos_sorted_by_amount: Mutex::new(vec![]),
            mempool_excluded_utxos: Mutex::new(HashMap::new()),
            used_outpoints: Mutex::new(HashMap::new()),
            first_sync_done: AtomicBool::new(false),
            coinbase_maturity,
            force_sync_sender: None,
        }
    }

    pub async fn is_synced(&self) -> bool {
        self.address_manager.lock().await.is_synced().await && self.first_sync_done.load(Relaxed)
    }

    pub fn start(sync_manager: Arc<Mutex<SyncManager>>) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = Self::sync_loop(sync_manager).await {
                panic!("Error in sync loop: {}", e);
            }
        })
    }
    pub async fn create_unsigned_transactions(
        &self,
        to_address: String,
        amount: u64,
        is_send_all: bool,
        payload: Vec<u8>,
        from_addresses_strings: Vec<String>,
        preselected_utxo_outpoints: Vec<Outpoint>,
        use_existing_change_address: bool,
        fee_policy: Option<FeePolicy>,
    ) -> Result<Vec<WalletSignableTransaction>, Box<dyn Error + Send + Sync>> {
        let validate_address =
            |address_string, name| -> Result<Address, Box<dyn Error + Send + Sync>> {
                match Address::try_from(address_string) {
                    Ok(address) => Ok(address),
                    Err(e) => Err(Box::new(UserInputError::new(format!(
                        "Invalid {} address: {}",
                        name, e
                    )))),
                }
            };

        let to_address = validate_address(to_address, "to")?;
        let address_set: HashMap<String, WalletAddress>;
        {
            let address_manager = self.address_manager.lock().await;
            address_set = address_manager.address_set().await;
        }

        if !from_addresses_strings.is_empty() && !preselected_utxo_outpoints.is_empty() {
            return Err(Box::new(UserInputError::new(
                "Cannot specify both from_addresses and utxos".to_string(),
            )));
        }

        let from_addresses = if from_addresses_strings.is_empty() {
            vec![]
        } else {
            let mut from_addresses = vec![];
            for address_string in from_addresses_strings {
                let wallet_address = address_set.get(&address_string).ok_or_else(|| {
                    UserInputError::new(format!(
                        "From address is not in address set: {}",
                        address_string
                    ))
                })?;
                from_addresses.push(wallet_address);
            }
            from_addresses
        };
        let preselected_utxos = if preselected_utxo_outpoints.is_empty() {
            HashMap::new()
        } else {
            let mut preselected_utxos = HashMap::new();
            {
                let utxos_sorted_by_amount = self.utxos_sorted_by_amount.lock().await;
                for outpoint in preselected_utxo_outpoints {
                    // TODO: index utxos by outpoint instead of searching all over an array
                    if let Some(utxo) = utxos_sorted_by_amount.iter().find(|utxo| {
                        utxo.outpoint.transaction_id.to_string() == outpoint.transaction_id
                            && utxo.outpoint.index == outpoint.index
                    }) {
                        let utxo = utxo.clone();
                        preselected_utxos.insert(utxo.outpoint.clone(), utxo);
                    } else {
                        return Err(Box::new(UserInputError::new(format!(
                            "UTXO {:?} is not in UTXO set",
                            outpoint
                        ))));
                    }
                }
                preselected_utxos
            }
        };

        let (fee_rate, max_fee) = self.calculate_fee_limits(fee_policy).await?;

        let change_address: Address;
        let change_wallet_address: WalletAddress;
        {
            let address_manager = self.address_manager.lock().await;
            (change_address, change_wallet_address) = // TODO: check if I really need both.
                address_manager.change_address(use_existing_change_address, &from_addresses)?;
        }

        let (selected_utxos, amount_sent_to_recipient, change_sompi) = self
            .select_utxos(
                preselected_utxos,
                HashSet::new(),
                amount,
                is_send_all,
                fee_rate,
                max_fee,
                from_addresses,
                &payload,
            )
            .await?;

        let mut payments = vec![WalletPayment::new(
            to_address.clone(),
            amount_sent_to_recipient,
        )];
        if change_sompi > 0 {
            payments.push(WalletPayment::new(change_address.clone(), change_sompi));
        }
        let unsigned_transaction = self
            .generate_unsigned_transaction(payments, payload, &selected_utxos)
            .await?;

        let unsigned_transactions = self
            .maybe_auto_compound_transaction(
                unsigned_transaction,
                to_address,
                change_address,
                change_wallet_address,
                fee_rate,
                max_fee,
            )
            .await?;

        Ok(unsigned_transactions)
    }

    async fn maybe_auto_compound_transaction(
        &self,
        unsigned_transaction: WalletSignableTransaction,
        to_address: Address,
        change_address: Address,
        change_wallet_address: WalletAddress,
        fee_rate: f64,
        max_fee: u64,
    ) -> Result<Vec<WalletSignableTransaction>, Box<dyn Error + Send + Sync>> {
        // TODO: implement actual splitting of transactions
        Ok(vec![unsigned_transaction])
    }

    async fn generate_unsigned_transaction(
        &self,
        payments: Vec<WalletPayment>,
        payload: Vec<u8>,
        selected_utxos: &Vec<WalletUtxo>,
    ) -> Result<WalletSignableTransaction, Box<dyn Error + Send + Sync>> {
        let mut sorted_extended_public_keys = self.keys.public_keys.clone();
        sorted_extended_public_keys.sort();

        let mut inputs = vec![];
        let mut utxo_entries = vec![];
        let mut derivation_paths = HashSet::new();
        for utxo in selected_utxos {
            let previous_outpoint =
                TransactionOutpoint::new(utxo.outpoint.transaction_id, utxo.outpoint.index);
            let input = TransactionInput::new(
                previous_outpoint,
                vec![],
                0,
                self.keys.minimum_signatures as u8,
            );
            inputs.push(input);

            let utxo_entry: UtxoEntry = utxo.utxo_entry.clone().into();
            utxo_entries.push(utxo_entry);
            {
                let address_manager = self.address_manager.lock().await;
                let derivation_path = address_manager.calculate_address_path(&utxo.address)?;
                derivation_paths.insert(derivation_path);
            }
        }

        let mut outputs = vec![];
        for payment in payments {
            let script_public_key = pay_to_address_script(&payment.address);
            let output = TransactionOutput::new(payment.amount, script_public_key);
            outputs.push(output);
        }

        let transaction = Transaction::new(0, inputs, outputs, 0, Default::default(), 0, payload);
        let signable_transaction = SignableTransaction::with_entries(transaction, utxo_entries);
        let wallet_signable_transaction = WalletSignableTransaction::new_from_unsigned(
            signable_transaction.clone(),
            derivation_paths,
        );

        Ok(wallet_signable_transaction)
    }
    async fn select_utxos(
        &self,
        preselected_utxos: HashMap<WalletOutpoint, WalletUtxo>,
        allowed_used_outpoints: HashSet<WalletOutpoint>,
        amount: u64,
        is_send_all: bool,
        fee_rate: f64,
        max_fee: u64,
        from_addresses: Vec<&WalletAddress>,
        payload: &Vec<u8>,
    ) -> Result<(Vec<WalletUtxo>, u64, u64), Box<dyn Error + Send + Sync>> {
        let mut total_value = 0;
        let mut selected_utxos = vec![];

        let dag_info = self.kaspa_rpc_client.get_block_dag_info().await?;

        let sync_manager = self;
        let mut fee = 0;
        let mut iteration = async |utxo: &WalletUtxo,
                                   avoid_preselected: bool|
               -> Result<bool, Box<dyn Error + Send + Sync>> {
            if !from_addresses.is_empty() && !from_addresses.contains(&&utxo.address) {
                return Ok(true);
            }
            if sync_manager.is_utxo_pending(utxo, dag_info.virtual_daa_score) {
                return Ok(true);
            }

            {
                let mut used_outpoints = sync_manager.used_outpoints.lock().await;
                if let Some(broadcast_time) = used_outpoints.get(&utxo.outpoint) {
                    if allowed_used_outpoints.contains(&utxo.outpoint) {
                        if sync_manager.has_used_outpoint_expired(broadcast_time).await {
                            used_outpoints.remove(&utxo.outpoint);
                        }
                    } else {
                        return Ok(true);
                    }
                }
            }

            if avoid_preselected {
                if preselected_utxos.contains_key(&utxo.outpoint) {
                    return Ok(true);
                }
            }

            selected_utxos.push(utxo.clone());
            total_value += utxo.utxo_entry.amount;
            let estimated_recipient_value = if is_send_all { total_value } else { amount };
            fee = self
                .estimate_fee(
                    &selected_utxos,
                    fee_rate,
                    max_fee,
                    estimated_recipient_value,
                    payload,
                )
                .await?;

            let total_spend = amount + fee;
            // Two break cases (if not send all):
            // 		1. total_value == totalSpend, so there's no change needed -> number of outputs = 1, so a single input is sufficient
            // 		2. total_value > totalSpend, so there will be change and 2 outputs, therefor in order to not struggle with --
            //		   2.1 go-nodes dust patch we try and find at least 2 inputs (even though the next one is not necessary in terms of spend value)
            // 		   2.2 KIP9 we try and make sure that the change amount is not too small
            if is_send_all {
                return Ok(true);
            }
            if total_value == total_spend {
                return Ok(false);
            }
            if total_value >= total_spend + MIN_CHANGE_TARGET && selected_utxos.len() > 1 {
                return Ok(false);
            }
            return Ok(true);
        };

        let mut should_continue = true;
        for (_, preselected_utxo) in &preselected_utxos {
            should_continue = iteration(preselected_utxo, false).await?;
            if !should_continue {
                break;
            };
        }
        if should_continue {
            {
                let utxos_sorted_by_amount = self.utxos_sorted_by_amount.lock().await;
                for utxo in utxos_sorted_by_amount.iter() {
                    should_continue = iteration(&utxo, false).await?;
                    if !should_continue {
                        break;
                    }
                }
            }
        }

        let total_spend: u64;
        let total_received: u64;
        if is_send_all {
            total_spend = total_value;
            total_received = total_value - fee;
        } else {
            total_spend = amount + fee;
            total_received = amount;
        }

        if total_value < total_spend {
            return Err(Box::new(UserInputError::new(format!(
                "Insufficient funds for send: {} required, while only {} available",
                amount / SOMPI_PER_KASPA,
                total_value / SOMPI_PER_KASPA
            ))));
        }

        Ok((selected_utxos, total_received, total_value - total_spend))
    }

    async fn estimate_fee(
        &self,
        selected_utxos: &Vec<WalletUtxo>,
        fee_rate: f64,
        max_fee: u64,
        estimated_recipient_value: u64,
        payload: &Vec<u8>,
    ) -> Result<u64, Box<dyn Error + Send + Sync>> {
        let estimated_mass = self
            .estimate_mass(selected_utxos, estimated_recipient_value, payload)
            .await?;
        let calculated_fee = ((estimated_mass as f64) * (fee_rate)).ceil() as u64;
        let fee = min(calculated_fee, max_fee);
        Ok(fee)
    }

    async fn estimate_mass(
        &self,
        selected_utxos: &Vec<WalletUtxo>,
        estimated_recipient_value: u64,
        payload: &Vec<u8>,
    ) -> Result<u64, Box<dyn Error + Send + Sync>> {
        let fake_public_key = &[0u8; 33];
        // We assume the worst case where the recipient address is ECDSA. In this case the scriptPubKey will be the longest.
        let fake_address = Address::new(self.address_prefix, Version::PubKeyECDSA, fake_public_key);

        let mut total_value = 0;
        for utxo in selected_utxos {
            total_value += utxo.utxo_entry.amount;
        }

        // This is an approximation for the distribution of value between the recipient output and the change output.
        let mock_payments = if total_value > estimated_recipient_value {
            vec![
                WalletPayment {
                    address: fake_address.clone(),
                    amount: estimated_recipient_value,
                },
                WalletPayment {
                    address: fake_address,
                    amount: total_value - estimated_recipient_value,
                },
            ]
        } else {
            vec![WalletPayment {
                address: fake_address,
                amount: total_value,
            }]
        };
        let mock_transaction = self
            .generate_unsigned_transaction(mock_payments, payload.clone(), selected_utxos)
            .await?;

        Ok(self
            .mass_calculator
            .calc_compute_mass_for_unsigned_consensus_transaction(
                &mock_transaction.transaction.unwrap().tx,
                self.keys.minimum_signatures,
            ))
    }

    pub async fn get_utxos_sorted_by_amount(&self) -> Vec<WalletUtxo> {
        self.utxos_sorted_by_amount.lock().await.clone()
    }

    pub async fn force_sync(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let force_sync_sender = &self.force_sync_sender;
        if let Some(sender) = force_sync_sender {
            if let Err(e) = sender.send(()).await {
                error!("Error sending to force sync channel: {}", e);
                // Do not return this error, sync will happen anyway
                // We don't want to disrupt operation because of this
            }
        } else {
            return Err(Box::new(UserInputError::new(
                "Force sync sender is not initialized".to_string(),
            )));
        }
        Ok(())
    }

    async fn sync_loop(
        sync_manager: Arc<Mutex<SyncManager>>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut force_sync_receiver: mpsc::Receiver<()>;
        let force_sync_sender: mpsc::Sender<()>;
        {
            let mut sync_manager = sync_manager.lock().await;
            (force_sync_sender, force_sync_receiver) = mpsc::channel(1);
            sync_manager.force_sync_sender = Some(force_sync_sender);

            info!("Starting sync loop");
            {
                let mut address_manager = sync_manager.address_manager.lock().await;
                address_manager.collect_recent_addresses().await?;
            }
            sync_manager.refresh_utxos().await?;
            sync_manager.first_sync_done.store(true, Relaxed);
            info!("Finished initial sync");
        }

        let mut interval = interval(core::time::Duration::from_secs(SYNC_INTERVAL));
        loop {
            tokio::select! {
                _ = interval.tick() => (),
                _ = force_sync_receiver.recv() => ()
            }

            {
                let mut sync_manager = sync_manager.lock().await;
                sync_manager.sync().await?;
            }
        }
    }

    async fn refresh_utxos(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        debug!("Refreshing UTXOs.");

        let refresh_start = Utc::now();

        let address_strings: Vec<String>;
        {
            let address_manager = self.address_manager.lock().await;
            address_strings = address_manager.address_strings().await?;
        }
        let rpc_addresses: Vec<RpcAddress> = address_strings
            .iter()
            .map(|address_string| Address::constructor(address_string))
            .collect();

        // It's important to check the mempool before calling `GetUTXOsByAddresses`:
        // If we would do it the other way around an output can be spent in the mempool
        // and not in consensus, and between the calls its spending transaction will be
        // added to consensus and removed from the mempool, so `getUTXOsByAddressesResponse`
        // will include an obsolete output.
        let mempool_entries_by_addresses = self
            .kaspa_rpc_client
            .get_mempool_entries_by_addresses(rpc_addresses.clone(), true, true)
            .await?;
        debug!("Got {} mempool entries", mempool_entries_by_addresses.len());

        let get_utxo_by_addresses_response = self
            .kaspa_rpc_client
            .get_utxos_by_addresses(rpc_addresses)
            .await?;
        debug!("Got {} utxo entries", get_utxo_by_addresses_response.len());

        self.update_utxo_set(
            get_utxo_by_addresses_response,
            mempool_entries_by_addresses,
            refresh_start,
        )
        .await
    }

    async fn sync(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        debug!("Starting sync cycle");
        {
            let mut address_manager = self.address_manager.lock().await;
            address_manager.collect_far_addresses().await?;
            address_manager.collect_recent_addresses().await?;
        }
        self.refresh_utxos().await?;

        debug!("Sync cycle completed successfully");

        Ok(())
    }

    async fn update_utxo_set(
        &self,
        rpc_utxo_entries: Vec<RpcUtxosByAddressesEntry>,
        rpc_mempool_utxo_entries: Vec<RpcMempoolEntryByAddress>,
        refresh_start_time: DateTime<Utc>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut wallet_utxos: Vec<WalletUtxo> = vec![];

        let mut exculde = HashSet::new();
        for rpc_mempool_entries_by_address in rpc_mempool_utxo_entries {
            for rpc_mempool_entry in rpc_mempool_entries_by_address.sending {
                for input in rpc_mempool_entry.transaction.inputs {
                    exculde.insert(input.previous_outpoint);
                }
            }
        }

        let mut mempool_excluded_utxos: HashMap<WalletOutpoint, WalletUtxo> = HashMap::new();
        {
            let address_set = self.address_manager.lock().await.address_set().await;

            for rpc_utxo_entry in rpc_utxo_entries {
                let wallet_outpoint: WalletOutpoint = rpc_utxo_entry.outpoint.into();
                let wallet_utxo_entry: WalletUtxoEntry = rpc_utxo_entry.utxo_entry.into();

                let rpc_address = rpc_utxo_entry.address.unwrap();
                let address = address_set.get(&rpc_address.address_to_string()).unwrap();

                let wallet_utxo =
                    WalletUtxo::new(wallet_outpoint, wallet_utxo_entry, address.clone());

                if exculde.contains(&rpc_utxo_entry.outpoint) {
                    mempool_excluded_utxos.insert(wallet_utxo.outpoint.clone(), wallet_utxo);
                } else {
                    wallet_utxos.push(wallet_utxo);
                }
            }
        }

        wallet_utxos.sort_by(|a, b| a.utxo_entry.amount.cmp(&b.utxo_entry.amount));
        *(self.start_time_of_last_completed_refresh.lock().await) = refresh_start_time;
        *(self.utxos_sorted_by_amount.lock().await) = wallet_utxos;
        *(self.mempool_excluded_utxos.lock().await) = mempool_excluded_utxos;

        // Cleanup expired used outpoints to avoid a memory leak
        let mut used_outpoints = self.used_outpoints.lock().await;
        for (outpoint, broadcast_time) in used_outpoints.clone() {
            if self.has_used_outpoint_expired(&broadcast_time).await {
                used_outpoints.remove(&outpoint);
            }
        }
        Ok(())
    }

    async fn has_used_outpoint_expired(&self, outpoint_broadcast_time: &DateTime<Utc>) -> bool {
        // If the node returns a UTXO we previously attempted to spend and enough time has passed, we assume
        // that the network rejected or lost the previous transaction and allow a reuse. We set this time
        // interval to a minute.
        // We also verify that a full refresh UTXO operation started after this time point and has already
        // completed, in order to make sure that indeed this state reflects a state obtained following the required wait time.
        return (self.start_time_of_last_completed_refresh.lock().await)
            .gt(&outpoint_broadcast_time.add(Duration::minutes(1)));
    }

    // returns:
    // 1. fee_rate
    // 2. max_fee
    async fn calculate_fee_limits(
        &self,
        fee_policy: Option<FeePolicy>,
    ) -> Result<(f64, u64), Box<dyn Error + Send + Sync>> {
        match fee_policy {
            Some(fee_policy) => match fee_policy.fee_policy {
                Some(fee_policy::FeePolicy::MaxFeeRate(requested_max_fee_rate)) => {
                    if requested_max_fee_rate < MIN_FEE_RATE {
                        return Err(Box::new(UserInputError::new(format!(
                            "requested max fee rate {} is too low, minimum fee rate is {}",
                            requested_max_fee_rate, MIN_FEE_RATE
                        ))));
                    }

                    let fee_estimate = self.kaspa_rpc_client.get_fee_estimate().await?;
                    let fee_rate = f64::min(
                        fee_estimate.normal_buckets[0].feerate,
                        requested_max_fee_rate,
                    );
                    Ok((fee_rate, u64::MAX))
                }
                Some(fee_policy::FeePolicy::ExactFeeRate(requested_exact_fee_rate)) => {
                    if requested_exact_fee_rate < MIN_FEE_RATE {
                        return Err(Box::new(UserInputError::new(format!(
                            "requested max fee rate {} is too low, minimum fee rate is {}",
                            requested_exact_fee_rate, MIN_FEE_RATE
                        ))));
                    }

                    Ok((requested_exact_fee_rate, u64::MAX))
                }
                Some(fee_policy::FeePolicy::MaxFee(requested_max_fee)) => {
                    let fee_estimate = self.kaspa_rpc_client.get_fee_estimate().await?;
                    Ok((fee_estimate.normal_buckets[0].feerate, requested_max_fee))
                }
                None => self.default_fee_rate().await,
            },
            None => self.default_fee_rate().await,
        }
    }

    async fn default_fee_rate(&self) -> Result<(f64, u64), Box<dyn Error + Send + Sync>> {
        let fee_estimate = self.kaspa_rpc_client.get_fee_estimate().await?;
        Ok((fee_estimate.normal_buckets[0].feerate, SOMPI_PER_KASPA)) // Default to a bound of max 1 KAS as fee
    }

    pub fn is_utxo_pending(&self, utxo: &WalletUtxo, virtual_daa_score: u64) -> bool {
        if !utxo.utxo_entry.is_coinbase {
            return false;
        }

        utxo.utxo_entry.block_daa_score + self.coinbase_maturity > virtual_daa_score
    }
}
