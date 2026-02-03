use crate::address_manager::{AddressManager, AddressQuerySet};
use crate::utxo_manager::UtxoManager;
use common::keys::Keys;
use kaspa_addresses::Address;
use kaspa_grpc_client::GrpcClient;
use kaspa_wallet_core::rpc::RpcApi;
use log::{debug, info};
use std::cmp::max;
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicU32};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::interval;

const NUM_INDEXES_TO_QUERY_FOR_FAR_ADDRESSES: u32 = 100;
const NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES: u32 = 1000;

pub struct SyncManager {
    kaspa_client: Arc<GrpcClient>,
    keys_file: Arc<Keys>,
    address_manager: Arc<Mutex<AddressManager>>,
    utxo_manager: Arc<UtxoManager>,

    sync_interval_millis: u64,
    first_sync_done: AtomicBool,
    next_sync_start_index: AtomicU32,
    recent_scan_next_index: AtomicU32,
    is_log_final_progress_line_shown: AtomicBool,
    max_used_addresses_for_log: AtomicU32,
    max_processed_addresses_for_log: AtomicU32,
}

impl SyncManager {
    pub fn new(
        kaspa_rpc_client: Arc<GrpcClient>,
        keys_file: Arc<Keys>,
        address_manager: Arc<Mutex<AddressManager>>,
        utxo_manager: Arc<UtxoManager>,
        sync_interval: u64,
    ) -> Self {
        Self {
            kaspa_client: kaspa_rpc_client,
            keys_file,
            address_manager,
            utxo_manager,
            sync_interval_millis: sync_interval,
            first_sync_done: AtomicBool::new(false),
            next_sync_start_index: 0.into(),
            recent_scan_next_index: 0.into(),
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

        let mut interval = interval(core::time::Duration::from_millis(self.sync_interval_millis));
        loop {
            interval.tick().await;

            {
                self.sync().await?;
            }
        }
    }

    async fn refresh_utxos(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        debug!("Refreshing UTXOs...");
        let monitored_addresses: Arc<Vec<Address>>;
        {
            let address_manager = self.address_manager.lock().await;
            monitored_addresses = address_manager.monitored_addresses().await?;
        }
        let addresses: Vec<Address> = monitored_addresses.as_ref().clone();

        if addresses.is_empty() {
            self.utxo_manager.update_utxo_set(vec![], vec![]).await?;
            return Ok(());
        }

        debug!("Getting mempool entries for addresses: {:?}...", addresses);
        // It's important to check the mempool before calling `GetUTXOsByAddresses`:
        // If we would do it the other way around an output can be spent in the mempool
        // and not in consensus, and between the calls its spending transaction will be
        // added to consensus and removed from the mempool, so `getUTXOsByAddressesResponse`
        // will include an obsolete output.
        let mempool_entries_by_addresses = self
            .kaspa_client
            .get_mempool_entries_by_addresses(addresses.clone(), true, true)
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

        debug!("Getting UTXOs by addresses...");
        let get_utxo_by_addresses_response =
            self.kaspa_client.get_utxos_by_addresses(addresses).await?;
        debug!("Got {} utxo entries", get_utxo_by_addresses_response.len());

        // `update_utxo_set` builds a new snapshot without holding any read locks and
        // swaps the Arc pointer under a brief lock.
        self.utxo_manager
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

        if !self.first_sync_done.load(Relaxed) {
            return self.collect_recent_addresses_full_scan().await;
        }
        self.collect_recent_addresses_incremental().await
    }

    async fn collect_recent_addresses_full_scan(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut index: u32 = 0;
        let mut max_used_index: u32 = 0;

        while index < max_used_index.saturating_add(NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES) {
            self.collect_addresses(index, index + NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES)
                .await?;
            index = index.saturating_add(NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES);

            max_used_index = self.last_used_index().await;

            self.update_address_collection_progress_log(index, max_used_index);
        }

        self.bump_next_sync_start_index(index);
        Ok(())
    }

    async fn collect_recent_addresses_incremental(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        // After the initial full scan, we avoid rescanning from index 0 on every tick. Instead we
        // scan a fixed-size chunk per cycle and advance a cursor, eventually covering the full
        // range [0, last_used_index + LOOKAHEAD).
        let last_used_index = self.last_used_index().await;
        let frontier = Self::recent_scan_frontier(last_used_index);

        // Keep "synced" semantics stable as last_used_index grows.
        self.bump_next_sync_start_index(frontier);

        let cursor = self.recent_scan_next_index.load(Relaxed);
        let (start, end, next_cursor) = Self::recent_scan_step(frontier, cursor);

        debug!(
            "Incremental recent-address scan: [{}, {}) (cursor={}, frontier={}, last_used_index={})",
            start, end, cursor, frontier, last_used_index
        );

        if start < end {
            self.collect_addresses(start, end).await?;
        }
        self.recent_scan_next_index.store(next_cursor, Relaxed);
        Ok(())
    }

    fn recent_scan_frontier(last_used_index: u32) -> u32 {
        last_used_index.saturating_add(NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES)
    }

    fn recent_scan_step(frontier: u32, cursor: u32) -> (u32, u32, u32) {
        if frontier == 0 {
            return (0, 0, 0);
        }
        let start = if cursor >= frontier { 0 } else { cursor };
        let end = start
            .saturating_add(NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES)
            .min(frontier);
        let next_cursor = if end >= frontier { 0 } else { end };
        (start, end, next_cursor)
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

    fn bump_next_sync_start_index(&self, candidate: u32) {
        let current = self.next_sync_start_index.load(Relaxed);
        if candidate > current {
            self.next_sync_start_index.store(candidate, Relaxed);
        }
    }

    async fn collect_addresses(
        &self,
        start: u32,
        end: u32,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        debug!("Collecting addresses from {} to {}", start, end);

        let addresses: AddressQuerySet;
        {
            let address_manager = self.address_manager.lock().await;
            addresses = address_manager.addresses_to_query(start, end).await?;
        }
        debug!("Querying {} addresses", addresses.len());

        let get_balances_by_addresses_response = self
            .kaspa_client
            .get_balances_by_addresses(
                addresses.keys().cloned().collect(),
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
                "{} addresses of {} processed ({:.2}%)",
                self.max_processed_addresses_for_log.load(Relaxed),
                self.max_used_addresses_for_log.load(Relaxed),
                percent_processed
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SyncManager;

    #[test]
    fn recent_scan_frontier_saturates_at_u32_max() {
        let lookahead = super::NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES;
        assert_eq!(SyncManager::recent_scan_frontier(u32::MAX), u32::MAX);
        assert_eq!(
            SyncManager::recent_scan_frontier(u32::MAX - lookahead + 1),
            u32::MAX
        );
    }

    #[test]
    fn recent_scan_step_clamps_end_to_frontier_and_wraps() {
        let lookahead = super::NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES;
        let frontier = lookahead / 2;
        let (start, end, next_cursor) = SyncManager::recent_scan_step(frontier, 0);
        assert_eq!(start, 0);
        assert_eq!(end, frontier);
        assert_eq!(next_cursor, 0);
    }

    #[test]
    fn recent_scan_step_advances_cursor_in_chunks_and_wraps() {
        let lookahead = super::NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES;
        let frontier = lookahead * 2 + 500;

        let (start1, end1, cursor1) = SyncManager::recent_scan_step(frontier, 0);
        assert_eq!((start1, end1, cursor1), (0, lookahead, lookahead));

        let (start2, end2, cursor2) = SyncManager::recent_scan_step(frontier, cursor1);
        assert_eq!(
            (start2, end2, cursor2),
            (lookahead, lookahead * 2, lookahead * 2)
        );

        let (start3, end3, cursor3) = SyncManager::recent_scan_step(frontier, cursor2);
        assert_eq!((start3, end3, cursor3), (lookahead * 2, frontier, 0));

        let (start4, end4, cursor4) = SyncManager::recent_scan_step(frontier, cursor3);
        assert_eq!((start4, end4, cursor4), (0, lookahead, lookahead));
    }

    #[test]
    fn recent_scan_step_resets_stale_cursor_to_zero() {
        let lookahead = super::NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES;
        let frontier = lookahead;
        let (start, end, next_cursor) = SyncManager::recent_scan_step(frontier, lookahead * 2);
        assert_eq!((start, end, next_cursor), (0, frontier, 0));
    }

    #[test]
    fn recent_scan_step_zero_frontier_is_empty() {
        assert_eq!(SyncManager::recent_scan_step(0, 0), (0, 0, 0));
        assert_eq!(SyncManager::recent_scan_step(0, 123), (0, 0, 0));
    }
}
