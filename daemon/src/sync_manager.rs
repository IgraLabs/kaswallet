use crate::address_manager::AddressManager;
use crate::model::{
    UserInputError, WalletAddress, WalletOutpoint, WalletPayment, WalletUtxo, WalletUtxoEntry,
};
use chrono::{DateTime, Duration, Utc};
use kaspa_addresses::Address;
use kaspa_consensus_core::constants::SOMPI_PER_KASPA;
use kaspa_consensus_core::tx::Transaction;
use kaspa_wrpc_client::prelude::{
    RpcAddress, RpcApi, RpcFeeEstimate, RpcMempoolEntryByAddress, RpcUtxosByAddressesEntry,
};
use kaspa_wrpc_client::KaspaRpcClient;
use log::{debug, error, info};
use std::cmp::min;
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
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

#[derive(Debug)]
pub struct SyncManager {
    kaspa_rpc_client: Arc<KaspaRpcClient>,
    address_manager: Arc<Mutex<AddressManager>>,

    start_time_of_last_completed_refresh: Mutex<DateTime<Utc>>,
    utxos_sorted_by_amount: Mutex<Vec<WalletUtxo>>,
    mempool_excluded_utxos: Mutex<HashMap<WalletOutpoint, WalletUtxo>>,
    used_outpoints: Mutex<HashMap<WalletOutpoint, DateTime<Utc>>>,
    first_sync_done: AtomicBool,

    force_sync_sender: mpsc::Sender<()>,
    force_sync_receiver: mpsc::Receiver<()>,
}

impl SyncManager {
    pub fn new(
        kaspa_rpc_client: Arc<KaspaRpcClient>,
        address_manager: Arc<Mutex<AddressManager>>,
    ) -> Self {
        let (force_sync_sender, force_sync_receiver) = mpsc::channel(1);

        Self {
            kaspa_rpc_client,
            address_manager,
            start_time_of_last_completed_refresh: Mutex::new(DateTime::<Utc>::MIN_UTC),
            utxos_sorted_by_amount: Mutex::new(vec![]),
            mempool_excluded_utxos: Mutex::new(HashMap::new()),
            used_outpoints: Mutex::new(HashMap::new()),
            first_sync_done: AtomicBool::new(false),
            force_sync_sender,
            force_sync_receiver,
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
        from_addresses: Vec<String>,
        utxos: Vec<Outpoint>,
        use_existing_change_address: bool,
        fee_policy: Option<FeePolicy>,
    ) -> Result<Vec<Transaction>, Box<dyn Error + Send + Sync>> {
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
        let from_addresses = match from_addresses.len() {
            0 => None,
            _ => {
                let mut addresses = vec![];
                for address_string in from_addresses {
                    let wallet_address = address_set.get(&address_string).ok_or_else(|| {
                        UserInputError::new(format!(
                            "From address is not in address set: {}",
                            address_string
                        ))
                    })?;
                    addresses.push(wallet_address);
                }
                Some(addresses)
            }
        };

        let (fee_rate, max_fee) = self.calculate_fee_limits(fee_policy).await?;

        let mut change_address: Address;
        let mut change_wallet_address: WalletAddress;
        {
            let address_manager = self.address_manager.lock().await;
            (change_address, change_wallet_address) = // TODO: check if I really need both.
                address_manager.change_address(use_existing_change_address, &from_addresses)?;
        }

        let (selected_utxos, spend_value, change_sompi) = self
            .select_utxos(amount, is_send_all, fee_rate, max_fee, from_addresses)
            .await?;

        let mut payments = vec![WalletPayment::new(to_address.clone(), amount)];
        if change_sompi > 0 {
            payments.push(WalletPayment::new(change_address.clone(), change_sompi));
        }
        let unsigned_transaction = self
            .generate_unsigned_transactions(payments, selected_utxos)
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
        unsigned_transaction: Transaction,
        to_address: Address,
        change_address: Address,
        change_wallet_address: WalletAddress,
        fee_rate: f64,
        max_fee: u64,
    ) -> Result<Vec<Transaction>, Box<dyn Error + Send + Sync>> {
        todo!()
    }

    async fn generate_unsigned_transactions(
        &self,
        payments: Vec<WalletPayment>,
        selected_utxos: Vec<WalletUtxo>,
    ) -> Result<Transaction, Box<dyn Error + Send + Sync>> {
        todo!()
    }
    async fn select_utxos(
        &self,
        amount: u64,
        is_send_all: bool,
        fee_rate: f64,
        max_fee: u64,
        from_addresses: Option<Vec<&WalletAddress>>,
    ) -> Result<(Vec<WalletUtxo>, u64, u64), Box<dyn Error + Send + Sync>> {
        todo!()
    }

    pub async fn get_utxos_sorted_by_amount(&self) -> Vec<WalletUtxo> {
        self.utxos_sorted_by_amount.lock().await.clone()
    }

    async fn sync_loop(
        sync_manager: Arc<Mutex<SyncManager>>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        {
            let sync_manager = sync_manager.lock().await;
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
            _ = interval.tick();

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

        let get_utxo_by_addresses_response = self
            .kaspa_rpc_client
            .get_utxos_by_addresses(rpc_addresses)
            .await?;

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
}
