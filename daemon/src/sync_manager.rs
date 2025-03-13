use crate::address_manager::AddressManager;
use crate::model::{WalletOutpoint, WalletUtxo, WalletUtxoEntry};
use chrono::{DateTime, Duration, Utc};
use kaspa_addresses::Address;
use kaspa_wrpc_client::prelude::{
    RpcAddress, RpcApi, RpcMempoolEntryByAddress, RpcUtxosByAddressesEntry,
};
use kaspa_wrpc_client::KaspaRpcClient;
use log::{debug, info};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::ops::Add;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use tokio::select;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio::time::interval;

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
        self.address_manager.lock().await.is_synced() && self.first_sync_done.load(Relaxed)
    }

    pub fn start(sync_manager: Arc<Mutex<SyncManager>>) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut sync_manager = sync_manager.lock().await;
            if let Err(e) = sync_manager.sync_loop().await {
                panic!("Error in sync loop: {}", e);
            }
        })
    }

    pub async fn sync_loop(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        info!("Starting sync loop");
        {
            let mut address_manager = self.address_manager.lock().await;
            address_manager.collect_recent_addresses().await?;
        }
        self.refresh_utxos().await?;
        self.first_sync_done.store(true, Relaxed);
        info!("Finished initial sync");

        let mut interval = interval(core::time::Duration::from_secs(1));
        loop {
            select! {
                _ = interval.tick() =>{}
                _ = self.force_sync_receiver.recv() => {}
            }
            self.sync().await?;
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
}
