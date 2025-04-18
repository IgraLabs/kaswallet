use crate::address_manager::{AddressManager, AddressSet};
use crate::utxo_manager::UtxoManager;
use common::keys::Keys;
use kaspa_addresses::Address;
use kaspa_wrpc_client::prelude::{RpcAddress, RpcApi};
use kaspa_wrpc_client::KaspaRpcClient;
use log::{debug, info};
use std::cmp::max;
use std::error::Error;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::interval;

const SYNC_INTERVAL: u64 = 10; // seconds

const NUM_INDEXES_TO_QUERY_FOR_FAR_ADDRESSES: u32 = 100;
const NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES: u32 = 1000;

pub struct SyncManager {
    kaspa_rpc_client: Arc<KaspaRpcClient>,
    keys_file: Arc<Keys>,
    address_manager: Arc<Mutex<AddressManager>>,
    utxo_manager: Arc<Mutex<UtxoManager>>,

    first_sync_done: AtomicBool,
    next_sync_start_index: AtomicU32,
    is_log_final_progress_line_shown: AtomicBool,
    max_used_addresses_for_log: AtomicU32,
    max_processed_addresses_for_log: AtomicU32,
}

impl SyncManager {
    pub fn new(
        kaspa_rpc_client: Arc<KaspaRpcClient>,
        keys_file: Arc<Keys>,
        address_manager: Arc<Mutex<AddressManager>>,
        utxo_manager: Arc<Mutex<UtxoManager>>,
    ) -> Self {
        Self {
            kaspa_rpc_client,
            keys_file,
            address_manager,
            utxo_manager,
            first_sync_done: AtomicBool::new(false),
            next_sync_start_index: 0.into(),
            is_log_final_progress_line_shown: false.into(),
            max_used_addresses_for_log: 0.into(),
            max_processed_addresses_for_log: 0.into(),
        }
    }

    pub async fn is_synced(&self) -> bool {
        self.next_sync_start_index.load(Relaxed) > self.last_used_index().await
            && self.first_sync_done.load(Relaxed)
    }

    async fn last_used_index(&self) -> u32 {
        let last_used_external_index = self.keys_file.last_used_external_index.load(Relaxed);
        let last_used_internal_index = self.keys_file.last_used_internal_index.load(Relaxed);

        max(last_used_external_index, last_used_internal_index)
    }

    pub fn start(sync_manager: Arc<SyncManager>) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = sync_manager.sync_loop().await {
                panic!("Error in sync loop: {}", e);
            }
        })
    }

    async fn sync_loop(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        {
            info!("Starting sync loop");
            self.collect_recent_addresses().await?;
            self.refresh_utxos().await?;
            self.first_sync_done.store(true, Relaxed);
            info!("Finished initial sync");
        }

        let mut interval = interval(core::time::Duration::from_secs(SYNC_INTERVAL));
        loop {
            interval.tick().await;

            {
                self.sync().await?;
            }
        }
    }

    async fn refresh_utxos(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        debug!("Refreshing UTXOs.");
        let address_strings: Vec<String>;
        {
            let address_manager = self.address_manager.lock().await;
            address_strings = address_manager.address_strings().await?;
        }
        let rpc_addresses: Vec<RpcAddress> = address_strings
            .iter()
            .map(|address_string| Address::constructor(address_string))
            .collect();

        // Lock utxo_manager at this stage, so that nobody tries to generate transactions while
        // we update the utxo set
        let mut utxo_manager = self.utxo_manager.lock().await;

        // It's important to check the mempool before calling `GetUTXOsByAddresses`:
        // If we would do it the other way around an output can be spent in the mempool
        // and not in consensus, and between the calls its spending transaction will be
        // added to consensus and removed from the mempool, so `getUTXOsByAddressesResponse`
        // will include an obsolete output.
        let mempool_entries_by_addresses = self
            .kaspa_rpc_client
            .get_mempool_entries_by_addresses(rpc_addresses.clone(), true, true)
            .await?;
        debug!(
            "Got {} mempool sending entries and {} receiving entries",
            mempool_entries_by_addresses
                .iter()
                .map(|me| me.sending.len())
                .sum::<usize>(),
            mempool_entries_by_addresses
                .iter()
                .map(|me| me.receiving.len())
                .sum::<usize>()
        );

        let get_utxo_by_addresses_response = self
            .kaspa_rpc_client
            .get_utxos_by_addresses(rpc_addresses)
            .await?;
        debug!("Got {} utxo entries", get_utxo_by_addresses_response.len());

        utxo_manager
            .update_utxo_set(get_utxo_by_addresses_response, mempool_entries_by_addresses)
            .await?;

        Ok(())
    }

    async fn sync(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        debug!("Starting sync cycle");
        {
            self.collect_far_addresses().await?;
            self.collect_recent_addresses().await?;
        }
        self.refresh_utxos().await?;

        debug!("Sync cycle completed successfully");

        Ok(())
    }

    pub async fn collect_recent_addresses(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        debug!("Collecting recent addresses");

        let mut index: u32 = 0;
        let mut max_used_index: u32 = 0;

        while index < max_used_index + NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES {
            let collect_addresses_result = self
                .collect_addresses(index, index + NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES)
                .await;
            if let Err(e) = collect_addresses_result {
                return Err(e);
            }
            index += NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES;

            max_used_index = self.last_used_index().await;

            self.update_address_collection_progress_log(index, max_used_index);
        }

        let next_sync_start_index = self.next_sync_start_index.load(Relaxed);
        if index > next_sync_start_index {
            self.next_sync_start_index.store(index, Relaxed);
        }
        Ok(())
    }

    pub async fn collect_far_addresses(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        debug!("Collecting far addresses");

        let next_sync_start_index = self.next_sync_start_index.load(Relaxed);

        self.collect_addresses(
            next_sync_start_index,
            next_sync_start_index + NUM_INDEXES_TO_QUERY_FOR_FAR_ADDRESSES,
        )
        .await?;

        self.next_sync_start_index
            .fetch_add(NUM_INDEXES_TO_QUERY_FOR_FAR_ADDRESSES, Relaxed);

        Ok(())
    }

    async fn collect_addresses(
        &self,
        start: u32,
        end: u32,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        debug!("Collecting addresses from {} to {}", start, end);

        let addresses: AddressSet;
        {
            let address_manager = self.address_manager.lock().await;
            addresses = address_manager.addresses_to_query(start, end).await?;
        }
        debug!("Querying {} addresses", addresses.len());

        let get_balances_by_addresses_response = self
            .kaspa_rpc_client
            .get_balances_by_addresses(
                addresses
                    .iter()
                    .map(|(address_string, _)| Address::constructor(address_string))
                    .collect(),
            )
            .await?;

        let address_manager = self.address_manager.lock().await;
        address_manager
            .update_addresses_and_last_used_indexes(addresses, get_balances_by_addresses_response)
            .await?;

        Ok(())
    }

    pub fn update_address_collection_progress_log(
        &self,
        processed_addresses: u32,
        max_used_addresses: u32,
    ) {
        if max_used_addresses > self.max_used_addresses_for_log.load(Relaxed) {
            self.max_used_addresses_for_log
                .store(max_used_addresses, Relaxed);
            if self.is_log_final_progress_line_shown.load(Relaxed) {
                info!("An additional set of previously used addresses found, processing...");
                self.max_processed_addresses_for_log.store(0, Relaxed);
                self.is_log_final_progress_line_shown.store(false, Relaxed);
            }
        }

        if processed_addresses > self.max_processed_addresses_for_log.load(Relaxed) {
            self.max_processed_addresses_for_log
                .store(processed_addresses, Relaxed)
        }

        if self.max_processed_addresses_for_log.load(Relaxed)
            >= self.max_used_addresses_for_log.load(Relaxed)
        {
            if !self.is_log_final_progress_line_shown.load(Relaxed) {
                info!("Finished scanning recent addresses");
                self.is_log_final_progress_line_shown.store(true, Relaxed);
            }
        } else {
            let percent_processed = self.max_processed_addresses_for_log.load(Relaxed) as f64
                / self.max_used_addresses_for_log.load(Relaxed) as f64
                * 100.0;

            info!(
                "{} addressed of {} of processed ({:.2}%)",
                self.max_processed_addresses_for_log.load(Relaxed),
                self.max_used_addresses_for_log.load(Relaxed),
                percent_processed
            );
        }
    }
}
