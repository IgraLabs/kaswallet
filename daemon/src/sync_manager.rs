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
use crate::utxo_manager::UtxoManager;

const SYNC_INTERVAL: u64 = 10; // seconds

// The current minimal fee rate according to mempool standards
const MIN_FEE_RATE: f64 = 1.0;

pub struct SyncManager {
    kaspa_rpc_client: Arc<KaspaRpcClient>,
    address_manager: Arc<Mutex<AddressManager>>,
    utxo_manager: Arc<Mutex<UtxoManager>>,
    keys: Arc<Keys>,
    address_prefix: AddressPrefix,

    start_time_of_last_completed_refresh: Mutex<DateTime<Utc>>,
    first_sync_done: AtomicBool,

    force_sync_sender: Option<mpsc::Sender<()>>,
}

impl SyncManager {
    pub fn new(
        network_id: NetworkId,
        kaspa_rpc_client: Arc<KaspaRpcClient>,
        address_manager: Arc<Mutex<AddressManager>>,
        utxo_manager: Arc<Mutex<UtxoManager>>,
        keys: Arc<Keys>,
        address_prefix: Prefix,
    ) -> Self {
        let network_params = NetworkParams::from(network_id);
        let coinbase_maturity = network_params
            .coinbase_transaction_maturity_period_daa
            .load(Relaxed);

        Self {
            kaspa_rpc_client,
            address_manager,
            utxo_manager,
            keys,
            address_prefix,
            start_time_of_last_completed_refresh: Mutex::new(DateTime::<Utc>::MIN_UTC),
            first_sync_done: AtomicBool::new(false),
            force_sync_sender: None, // TODO: re-establish force-sync
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

        let mut utxo_manager = self.utxo_manager.lock().await;
        utxo_manager.update_utxo_set(
            get_utxo_by_addresses_response,
            mempool_entries_by_addresses,
        )
        .await?;

        *(self.start_time_of_last_completed_refresh.lock().await) = refresh_start_time;
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

}
