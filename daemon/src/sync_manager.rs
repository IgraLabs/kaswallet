use crate::address_manager::{AddressManager, AddressSet};
use crate::utxo_manager::UtxoManager;
use common::error_location::ErrorLocation;
use common::errors::{RpcError, SyncError, WalletError, WalletResult};
use common::keys::Keys;
use common::status_classify::classify_kaspad_rpc_error;
use kaspa_addresses::Address;
use kaspa_grpc_client::GrpcClient;
use kaspa_wallet_core::rpc::RpcApi;
use std::cmp::max;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicU32};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::{Duration, MissedTickBehavior, interval, sleep};
use tracing::{debug, info, warn};

const NUM_INDEXES_TO_QUERY_FOR_FAR_ADDRESSES: u32 = 100;
const NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES: u32 = 1000;

pub struct SyncManager {
    kaspa_client: Arc<GrpcClient>,
    keys_file: Arc<Keys>,
    address_manager: Arc<Mutex<AddressManager>>,
    utxo_manager: Arc<Mutex<UtxoManager>>,

    sync_interval_millis: u64,
    first_sync_done: AtomicBool,
    utxos_fresh: AtomicBool,
    next_sync_start_index: AtomicU32,
    is_log_final_progress_line_shown: AtomicBool,
    max_used_addresses_for_log: AtomicU32,
    max_processed_addresses_for_log: AtomicU32,
}

impl SyncManager {
    pub fn new(
        kaspa_rpc_client: Arc<GrpcClient>,
        keys_file: Arc<Keys>,
        address_manager: Arc<Mutex<AddressManager>>,
        utxo_manager: Arc<Mutex<UtxoManager>>,
        sync_interval: u64,
    ) -> Self {
        Self {
            kaspa_client: kaspa_rpc_client,
            keys_file,
            address_manager,
            utxo_manager,
            sync_interval_millis: sync_interval,
            first_sync_done: AtomicBool::new(false),
            utxos_fresh: AtomicBool::new(false),
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

    pub(crate) fn utxos_are_fresh(&self) -> bool {
        self.utxos_fresh.load(Relaxed)
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

    /// One-shot address discovery + UTXO load, marking the wallet synced.
    /// Used as the initial pass of the daemon's sync loop and by standalone
    /// binaries (e.g. `kaswallet-batch-send`) that need a synced wallet view
    /// without running the background loop. Mirrors daemon startup exactly:
    /// like the initial sync, it does not run `collect_far_addresses`, so
    /// wallets with used-address gaps beyond the recent-scan window should
    /// sync a daemon first.
    pub async fn sync_once(&self) -> WalletResult<()> {
        self.collect_recent_addresses().await?;
        self.refresh_utxos().await?;
        self.first_sync_done.store(true, Relaxed);
        Ok(())
    }

    async fn sync_loop(&self) -> WalletResult<()> {
        run_sync_loop(
            |initial| async move {
                if initial {
                    self.sync_once().await
                } else {
                    self.sync().await
                }
            },
            || self.kaspa_client.is_connected(),
            Duration::from_millis(self.sync_interval_millis),
        )
        .await
    }

    async fn refresh_utxos(&self) -> WalletResult<()> {
        debug!("Refreshing UTXOs...");
        let address_strings: Vec<String>;
        {
            let address_manager = self.address_manager.lock().await;
            address_strings = address_manager.address_strings().await?;
        }
        let addresses: Vec<Address> = address_strings
            .iter()
            .map(|address_string| Address::constructor(address_string))
            .collect();

        // Lock utxo_manager at this stage, so that nobody tries to generate transactions while
        // we update the utxo set
        let mut utxo_manager = self.utxo_manager.lock().await;
        self.utxos_fresh.store(false, Relaxed);

        debug!("Getting mempool entries for addresses: {:?}...", addresses);
        let addresses_count = addresses.len();
        let mempool_entries_by_addresses = self
            .kaspa_client
            .get_mempool_entries_by_addresses(addresses.clone(), true, true)
            .await
            .map_err(|e| sync_rpc_error("get_mempool_entries_by_addresses", addresses_count, e))?;
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
        let get_utxo_by_addresses_response = self
            .kaspa_client
            .get_utxos_by_addresses(addresses)
            .await
            .map_err(|e| sync_rpc_error("get_utxos_by_addresses", addresses_count, e))?;
        debug!("Got {} utxo entries", get_utxo_by_addresses_response.len());

        utxo_manager
            .update_utxo_set(get_utxo_by_addresses_response, mempool_entries_by_addresses)
            .await?;
        self.utxos_fresh.store(true, Relaxed);

        // Surface a per-sync summary of what we now hold, split by
        // confirmed vs. mempool-receiving. The two counts let an operator
        // tell at a glance whether the wallet is sourcing inputs from
        // kaspad's consensus UTXO set or from ephemeral mempool outputs
        // (only the former survive a reorg → orphan).
        let snapshot = utxo_manager.utxos_by_outpoint();
        let confirmed = snapshot
            .values()
            .filter(|u| !u.utxo_entry.is_unconfirmed)
            .count();
        let unconfirmed = snapshot.len().saturating_sub(confirmed);
        info!(
            confirmed,
            unconfirmed,
            total = snapshot.len(),
            "utxo set refreshed"
        );

        Ok(())
    }

    async fn sync(&self) -> WalletResult<()> {
        debug!("Starting sync cycle");
        {
            self.collect_far_addresses().await?;
            self.collect_recent_addresses().await?;
        }
        self.refresh_utxos().await?;

        debug!("Sync cycle completed successfully");

        Ok(())
    }

    pub async fn collect_recent_addresses(&self) -> WalletResult<()> {
        debug!("Collecting recent addresses");

        let mut index: u32 = 0;
        let mut max_used_index: u32 = 0;

        while index < max_used_index + NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES {
            self.collect_addresses(index, index + NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES)
                .await?;
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

    pub async fn collect_far_addresses(&self) -> WalletResult<()> {
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

    async fn collect_addresses(&self, start: u32, end: u32) -> WalletResult<()> {
        debug!("Collecting addresses from {} to {}", start, end);

        let addresses: AddressSet;
        {
            let address_manager = self.address_manager.lock().await;
            addresses = address_manager.addresses_to_query(start, end).await?;
        }
        debug!("Querying {} addresses", addresses.len());

        let addresses_count = addresses.len();
        let get_balances_by_addresses_response = self
            .kaspa_client
            .get_balances_by_addresses(
                addresses
                    .keys()
                    .map(|address_string| Address::constructor(address_string))
                    .collect(),
            )
            .await;
        let get_balances_by_addresses_response = match get_balances_by_addresses_response {
            Ok(response) => response,
            Err(error) => {
                let _utxo_guard = self.utxo_manager.lock().await;
                self.utxos_fresh.store(false, Relaxed);
                return Err(sync_rpc_error(
                    "get_balances_by_addresses",
                    addresses_count,
                    error,
                ));
            }
        };

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

#[track_caller]
fn sync_rpc_error(
    operation: &'static str,
    addresses_count: usize,
    error: kaspa_rpc_core::RpcError,
) -> WalletError {
    // The pinned gRPC client flattens transport and remote errors into General.
    // Classify the raw message before sanitizing it so only exact matches retry.
    let transient = matches!(
        &error,
        kaspa_rpc_core::RpcError::General(message)
            if message == "RPC request timeout"
                || message == "consensus is currently in a transitional ibd state"
    ) || matches!(
        error,
        kaspa_rpc_core::RpcError::ConsensusInTransitionalIbdState
    );
    let mut source = classify_kaspad_rpc_error(operation, error);
    if transient && let RpcError::KaspadStatus { code, .. } = &mut source {
        // This is our transient classification, not a status from the wire.
        *code = tonic::Code::Unavailable;
    }
    SyncError::UtxoFetchFailed {
        addresses_count,
        source: Box::new(source),
        location: ErrorLocation::capture(),
    }
    .into()
}

async fn retry_sync<F, Fut>(mut sync: F, is_connected: impl Fn() -> bool) -> WalletResult<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = WalletResult<()>>,
{
    let mut delay = Duration::from_secs(1);
    loop {
        match sync().await {
            Ok(()) => return Ok(()),
            Err(error) => {
                let transient = matches!(
                    &error,
                    WalletError::Sync(SyncError::UtxoFetchFailed { source, .. })
                        if matches!(source.as_ref(), RpcError::KaspadStatus {
                            code: tonic::Code::Unavailable, ..
                        })
                );
                if !transient || !is_connected() {
                    return Err(error);
                }
                warn!(%error, retry_delay_secs = delay.as_secs(), "Sync RPC failed; retrying full pass");
                sleep(delay).await;
                if !is_connected() {
                    warn!(%error, "Kaspad disconnected during sync backoff; stopping sync loop");
                    return Err(error);
                }
                delay = (delay * 2).min(Duration::from_secs(60));
            }
        }
    }
}

async fn run_sync_loop<F, Fut>(
    mut sync: F,
    is_connected: impl Fn() -> bool,
    period: Duration,
) -> WalletResult<()>
where
    F: FnMut(bool) -> Fut,
    Fut: Future<Output = WalletResult<()>>,
{
    info!("Starting sync loop");
    retry_sync(|| sync(true), &is_connected).await?;
    info!("Finished initial sync");

    let mut ticks = interval(period);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        retry_sync(|| sync(false), &is_connected).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::errors::{ConfigError, StorageError};
    use tokio::sync::mpsc;
    use tokio::time::{Instant, advance};

    fn rpc_failure(message: &str) -> WalletError {
        sync_rpc_error(
            "get_utxos_by_addresses",
            2000,
            kaspa_rpc_core::RpcError::General(message.into()),
        )
    }

    fn config_failure() -> WalletError {
        ConfigError::MissingArgument {
            name: "--rpc-server",
            location: ErrorLocation::capture(),
        }
        .into()
    }

    #[tokio::test(start_paused = true)]
    async fn pinned_rpc_error_conversions_remain_retryable() {
        for error in [
            kaspa_grpc_client::error::Error::Timeout.into(),
            kaspa_rpc_core::RpcError::ConsensusInTransitionalIbdState,
            kaspa_rpc_core::RpcError::ConsensusInTransitionalIbdState
                .to_string()
                .into(),
        ] {
            let mut failure = Some(sync_rpc_error("get_utxos_by_addresses", 2000, error));
            let mut attempts = 0;
            let started = Instant::now();
            retry_sync(
                || {
                    attempts += 1;
                    let result = match failure.take() {
                        Some(error) => Err(error),
                        None => Ok(()),
                    };
                    async move { result }
                },
                || true,
            )
            .await
            .unwrap();
            assert_eq!(attempts, 2);
            assert_eq!(started.elapsed(), Duration::from_secs(1));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn virtual_time_retries_initial_and_periodic_sync_without_bursts() {
        let started = Instant::now();
        let (sender, mut attempts) = mpsc::unbounded_channel();
        let mut initial_attempts = 0;
        let mut periodic_attempts = 0;
        let task = tokio::spawn(run_sync_loop(
            move |initial| {
                sender.send((initial, started.elapsed())).unwrap();
                let result = if initial {
                    initial_attempts += 1;
                    if initial_attempts <= 8 {
                        Err(rpc_failure("RPC request timeout"))
                    } else {
                        Ok(())
                    }
                } else {
                    periodic_attempts += 1;
                    match periodic_attempts {
                        1..=6 => Err(rpc_failure(
                            "consensus is currently in a transitional ibd state",
                        )),
                        7..=8 => Ok(()),
                        _ => Err(config_failure()),
                    }
                };
                async move { result }
            },
            || true,
            Duration::from_secs(10),
        ));

        assert_eq!(attempts.recv().await.unwrap(), (true, Duration::ZERO));
        let mut elapsed = Duration::ZERO;
        for (initial, delays) in [
            (true, &[1, 2, 4, 8, 16, 32, 60, 60][..]),
            (false, &[1, 2, 4, 8, 16, 32][..]),
        ] {
            for delay in delays {
                let delay = Duration::from_secs(*delay);
                advance(delay - Duration::from_millis(1)).await;
                tokio::task::yield_now().await;
                assert!(
                    attempts.try_recv().is_err(),
                    "retry started before its pause"
                );
                advance(Duration::from_millis(1)).await;
                elapsed += delay;
                assert_eq!(attempts.recv().await.unwrap(), (initial, elapsed));
            }
            // Preserve the immediate first periodic tick. After a long retry,
            // Delay permits one overdue tick, then schedules the next period.
            assert_eq!(attempts.recv().await.unwrap(), (false, elapsed));
        }

        advance(Duration::from_millis(9999)).await;
        tokio::task::yield_now().await;
        assert!(
            attempts.try_recv().is_err(),
            "accumulated ticks caused a burst"
        );
        advance(Duration::from_millis(1)).await;
        assert_eq!(
            attempts.recv().await.unwrap(),
            (false, elapsed + Duration::from_secs(10))
        );
        assert!(matches!(task.await.unwrap(), Err(WalletError::Config(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn non_transient_or_disconnected_errors_are_not_retried() {
        let mut cases = vec![
            (config_failure(), true),
            (
                StorageError::Io {
                    path: "wallet.keys".into(),
                    reason: "disk full".into(),
                    location: ErrorLocation::capture(),
                }
                .into(),
                true,
            ),
            (rpc_failure("RPC request timeout"), false),
            (
                rpc_failure("consensus is currently in a transitional ibd state"),
                false,
            ),
        ];
        for message in [
            "Not connected to server",
            "RPC request timeout: invalid configuration",
            "RPC request timeout ",
            "RPC request time\0out",
            "consensus is currently in a transitional ibd state: permanent failure",
        ] {
            cases.push((rpc_failure(message), true));
        }

        for (error, connected) in cases {
            let expected = error.to_string();
            let started = Instant::now();
            let mut error = Some(error);
            let mut attempts = 0;
            let result = retry_sync(
                || {
                    attempts += 1;
                    let error = error.take().expect("non-retryable failure was retried");
                    async move { Err(error) }
                },
                || connected,
            )
            .await;
            assert_eq!(result.unwrap_err().to_string(), expected);
            assert_eq!(attempts, 1);
            assert_eq!(started.elapsed(), Duration::ZERO);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn disconnect_during_backoff_stops_without_another_pass() {
        let connected = Arc::new(AtomicBool::new(true));
        let connection = connected.clone();
        let (sender, mut attempts) = mpsc::unbounded_channel();
        let task = tokio::spawn(retry_sync(
            move || {
                sender.send(()).unwrap();
                async { Err(rpc_failure("RPC request timeout")) }
            },
            move || connection.load(Relaxed),
        ));
        attempts.recv().await.unwrap();
        connected.store(false, Relaxed);
        advance(Duration::from_secs(1)).await;
        assert!(matches!(task.await.unwrap(), Err(WalletError::Sync(_))));
        assert!(attempts.try_recv().is_err());
    }
}
