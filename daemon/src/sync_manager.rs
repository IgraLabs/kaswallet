use crate::address_manager::AddressManager;
use crate::transaction_generator::TransactionGenerator;
use crate::utxo_manager::UtxoManager;
use chrono::Utc;
use common::errors::WalletError;
use kaspa_addresses::Address;
use kaspa_wrpc_client::prelude::{RpcAddress, RpcApi};
use kaspa_wrpc_client::KaspaRpcClient;
use log::{debug, error, info};
use std::error::Error;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio::time::interval;

const SYNC_INTERVAL: u64 = 10; // seconds

pub struct SyncManager {
    kaspa_rpc_client: Arc<KaspaRpcClient>,
    address_manager: Arc<Mutex<AddressManager>>,
    transaction_generator: Arc<Mutex<TransactionGenerator>>,
    utxo_manager: Arc<Mutex<UtxoManager>>,

    first_sync_done: AtomicBool,

    force_sync_sender: Option<mpsc::Sender<()>>,
}

impl SyncManager {
    pub fn new(
        kaspa_rpc_client: Arc<KaspaRpcClient>,
        address_manager: Arc<Mutex<AddressManager>>,
        utxo_manager: Arc<Mutex<UtxoManager>>,
        transaction_generator: Arc<Mutex<TransactionGenerator>>,
    ) -> Self {
        Self {
            kaspa_rpc_client,
            address_manager,
            transaction_generator,
            utxo_manager,
            first_sync_done: AtomicBool::new(false),
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
    pub async fn force_sync(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        debug!("Force sync called!");

        let force_sync_sender = &self.force_sync_sender;
        if let Some(sender) = force_sync_sender {
            if let Err(e) = sender.send(()).await {
                error!("Error sending to force sync channel: {}", e);
                // Do not return this error, sync will happen anyway
                // We don't want to disrupt operation because of this
            }
        } else {
            return Err(Box::new(WalletError::SanityCheckFailed(
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

    async fn refresh_utxos(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        debug!("Refreshing UTXOs.");

        let refresh_start_time = Utc::now();

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

        {
            let mut utxo_manager = self.utxo_manager.lock().await;
            utxo_manager
                .update_utxo_set(
                    get_utxo_by_addresses_response,
                    mempool_entries_by_addresses,
                    refresh_start_time,
                )
                .await?;
        }
        {
            let mut transaction_generator = self.transaction_generator.lock().await;
            transaction_generator.cleanup_expired_used_outpoints().await;
        }

        Ok(())
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
}
