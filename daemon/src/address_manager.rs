use crate::model::{
    Keychain, WalletAddress, WalletOutpoint, WalletUtxo, WalletUtxoEntry, KEYCHAINS,
};
use chrono::prelude::*;
use chrono::Duration;
use common::keys::Keys;
use kaspa_addresses::{Address, Prefix as AddressPrefix, Version as AddressVersion};
use kaspa_bip32::secp256k1::PublicKey;
use kaspa_bip32::{DerivationPath, ExtendedPublicKey};
use kaspa_wrpc_client::prelude::{
    RpcAddress, RpcApi, RpcBalancesByAddressesEntry, RpcMempoolEntryByAddress,
    RpcUtxosByAddressesEntry,
};
use kaspa_wrpc_client::KaspaRpcClient;
use log::info;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::ops::Add;
use std::str::FromStr;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use tokio::select;
use tokio::sync::{mpsc, Mutex};
use tokio::time::interval;
use tonic::transport::Channel;
use wallet_proto::wallet_proto::wallet_server::Wallet;

const NUM_INDEXES_TO_QUERY_FOR_FAR_ADDRESSES: u32 = 100;
const NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES: u32 = 1000;

type AddressSet = HashMap<String, WalletAddress>;
#[derive(Debug)]
pub struct AddressManager {
    keys_file: Arc<Keys>,
    extended_public_keys: Arc<Vec<ExtendedPublicKey<PublicKey>>>,
    addresses: Mutex<AddressSet>,
    next_sync_start_index: Mutex<u32>,
    kaspa_rpc_client: Arc<KaspaRpcClient>,
    is_multisig: bool,
    prefix: AddressPrefix,

    is_log_final_progress_line_shown: bool,
    max_used_addresses_for_log: u32,
    max_processed_addresses_for_log: u32,

    start_time_of_last_completed_refresh: Mutex<DateTime<Utc>>,
    utxos_sorted_by_amount: Mutex<Vec<WalletUtxo>>,
    mempool_excluded_utxos: Mutex<HashMap<WalletOutpoint, WalletUtxo>>,
    used_outpoints: Mutex<HashMap<WalletOutpoint, DateTime<Utc>>>,
    first_sync_done: AtomicBool,

    force_sync_sender: mpsc::Sender<()>,
    force_sync_receiver: mpsc::Receiver<()>,
}

impl AddressManager {
    pub fn new(
        kaspa_rpc_client: Arc<KaspaRpcClient>,
        keys: Arc<Keys>,
        prefix: AddressPrefix,
    ) -> Self {
        let is_multisig = keys.public_keys.len() > 0;
        let (force_sync_sender, force_sync_receiver) = mpsc::channel(32);

        Self {
            kaspa_rpc_client,
            keys_file: keys.clone(),
            extended_public_keys: Arc::new(keys.public_keys.clone()),
            addresses: Mutex::new(HashMap::new()),
            next_sync_start_index: Mutex::new(0),
            is_multisig,
            prefix,
            is_log_final_progress_line_shown: false,
            max_used_addresses_for_log: 0,
            max_processed_addresses_for_log: 0,
            start_time_of_last_completed_refresh: Mutex::new(DateTime::<Utc>::MIN_UTC),
            utxos_sorted_by_amount: Mutex::new(vec![]),
            mempool_excluded_utxos: Mutex::new(HashMap::new()),
            used_outpoints: Mutex::new(HashMap::new()),
            first_sync_done: AtomicBool::new(false),
            force_sync_sender,
            force_sync_receiver,
        }
    }

    pub async fn sync_loop(&mut self) -> Result<(), Box<dyn Error>> {
        self.collect_recent_addresses().await?;
        self.refresh_utxos().await?;
        self.first_sync_done.store(true, Relaxed);

        let mut interval = interval(core::time::Duration::from_secs(1));
        loop {
            select! {
                _ = interval.tick() =>{}
                _ = self.force_sync_receiver.recv() => {}
            }
            self.sync().await?;
        }
    }

    pub async fn address_strings(&self) -> Result<Vec<String>, Box<dyn Error>> {
        let addresses = self.addresses.lock().await;
        let strings = addresses
            .keys()
            .map(|address_string| address_string.to_string())
            .collect();

        Ok(strings)
    }

    pub async fn collect_recent_addresses(&mut self) -> Result<(), Box<dyn Error>> {
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

            max_used_index = self.max_used_index().await;

            self.update_syncing_progress_log(index, max_used_index);
        }

        let mut next_sync_start_index = self.next_sync_start_index.lock().await;
        if index > *next_sync_start_index {
            *next_sync_start_index = index;
        }
        Ok(())
    }

    pub async fn collect_far_addresses(&mut self) -> Result<(), Box<dyn Error>> {
        let mut next_sync_start_index = self.next_sync_start_index.lock().await;

        self.collect_addresses(
            *next_sync_start_index,
            *next_sync_start_index + NUM_INDEXES_TO_QUERY_FOR_FAR_ADDRESSES,
        )
        .await?;

        *next_sync_start_index += NUM_INDEXES_TO_QUERY_FOR_FAR_ADDRESSES;

        Ok(())
    }

    async fn collect_addresses(&self, start: u32, end: u32) -> Result<(), Box<dyn Error>> {
        let addresses = self.addresses_to_query(start, end)?;

        let get_balances_by_addresses_response = self
            .kaspa_rpc_client
            .get_balances_by_addresses(
                addresses
                    .iter()
                    .map(|(address_string, _)| Address::constructor(address_string))
                    .collect(),
            )
            .await?;

        self.update_addresses_and_last_used_indexes(addresses, get_balances_by_addresses_response)
            .await?;

        Ok(())
    }

    fn addresses_to_query(&self, start: u32, end: u32) -> Result<AddressSet, Box<dyn Error>> {
        let mut addresses = HashMap::new();

        for index in start..end {
            for cosigner_index in 0..self.extended_public_keys.len() as u32 {
                for keychain in KEYCHAINS {
                    let wallet_address = WalletAddress {
                        index,
                        cosigner_index,
                        keychain,
                    };
                    let address = self.calculate_address(&wallet_address)?;
                    addresses.insert(address.to_string(), wallet_address);
                }
            }
        }

        Ok(addresses)
    }

    async fn update_addresses_and_last_used_indexes(
        &self,
        address_set: AddressSet,
        get_balances_by_addresses_response: Vec<RpcBalancesByAddressesEntry>,
    ) -> Result<(), Box<dyn Error>> {
        let mut last_used_external_index = self.keys_file.last_used_external_index.lock().await;
        let mut last_used_internal_index = self.keys_file.last_used_internal_index.lock().await;

        for entry in get_balances_by_addresses_response {
            if entry.balance == None || entry.balance == Some(0) {
                // TODO: Check if it's actually None or Some(0)
                continue;
            }

            let address_string = entry.address.to_string();
            let wallet_address = address_set.get(&address_string).unwrap();

            self.addresses
                .lock()
                .await
                .insert(address_string, wallet_address.clone());

            if wallet_address.keychain == Keychain::External {
                if wallet_address.index > *last_used_external_index {
                    *last_used_external_index = wallet_address.index;
                }
            } else {
                if wallet_address.index > *last_used_internal_index {
                    *last_used_internal_index = wallet_address.index;
                }
            }
        }

        self.keys_file.save()?;

        Ok(())
    }

    fn calculate_address(&self, wallet_address: &WalletAddress) -> Result<Address, Box<dyn Error>> {
        let path = self.calculate_address_path(wallet_address)?;

        if self.is_multisig {
            self.p2pk_address(path)
        } else {
            self.multisig_address(path)
        }
    }

    fn calculate_address_path(
        &self,
        wallet_address: &WalletAddress,
    ) -> Result<DerivationPath, Box<dyn Error>> {
        let wallet_address = wallet_address.clone();
        let path_string = if self.is_multisig {
            format!(
                "m/{}/{}/{}",
                wallet_address.cosigner_index, wallet_address.keychain as u32, wallet_address.index
            )
        } else {
            format!(
                "m/{}/{}",
                wallet_address.keychain as u32, wallet_address.index
            )
        };

        let path = DerivationPath::from_str(&path_string)?;
        Ok(path)
    }

    fn p2pk_address(&self, derivation_path: DerivationPath) -> Result<Address, Box<dyn Error>> {
        let extended_public_key = self.extended_public_keys.first().unwrap().clone();
        let derived_key = extended_public_key.derive_path(&derivation_path)?;
        let pk = derived_key.public_key();
        let payload = pk.x_only_public_key().0.serialize();
        let address = Address::new(self.prefix, AddressVersion::PubKey, &payload);
        Ok(address)
    }

    fn multisig_address(&self, derivation_path: DerivationPath) -> Result<Address, Box<dyn Error>> {
        let mut sorted_extended_public_keys = self.extended_public_keys.as_ref().clone();
        sorted_extended_public_keys.sort();

        let mut public_keys = vec![];
        for x_public_key in sorted_extended_public_keys.iter() {
            let derived_key = x_public_key.clone().derive_path(&derivation_path)?;
            let public_key = derived_key.public_key();
            public_keys.push(public_key.x_only_public_key().0.serialize());
        }

        let redeem_script = kaspa_txscript::multisig_redeem_script(
            public_keys.iter(),
            self.keys_file.minimum_signatures as usize,
        )?;
        let script_pub_key = kaspa_txscript::pay_to_script_hash_script(redeem_script.as_slice());
        let address = kaspa_txscript::extract_script_pub_key_address(&script_pub_key, self.prefix)?;
        Ok(address)
    }

    async fn max_used_index(&self) -> u32 {
        let last_used_external_index = *self.keys_file.last_used_external_index.lock().await;
        let last_used_internal_index = *self.keys_file.last_used_internal_index.lock().await;

        if last_used_external_index > last_used_internal_index {
            last_used_external_index
        } else {
            last_used_internal_index
        }
    }

    fn update_syncing_progress_log(&mut self, processed_addresses: u32, max_used_addresses: u32) {
        if max_used_addresses > self.max_used_addresses_for_log {
            self.max_used_addresses_for_log = max_used_addresses;
            if self.is_log_final_progress_line_shown {
                info!("An additional set of previously used addresses found, processing...");
                self.max_processed_addresses_for_log = 0;
                self.is_log_final_progress_line_shown = false;
            }
        }

        if processed_addresses > self.max_processed_addresses_for_log {
            self.max_processed_addresses_for_log = processed_addresses
        }

        if self.max_processed_addresses_for_log >= self.max_used_addresses_for_log {
            if !self.is_log_final_progress_line_shown {
                info!("Finished scanning recent addresses");
                self.is_log_final_progress_line_shown = true;
            }
        } else {
            let percent_processed = self.max_processed_addresses_for_log as f64
                / self.max_used_addresses_for_log as f64
                * 100.0;

            info!(
                "{} addressed of {} of processed ({:.2})",
                self.max_processed_addresses_for_log,
                self.max_used_addresses_for_log,
                percent_processed
            );
        }
    }

    async fn refresh_utxos(&self) -> Result<(), Box<dyn Error>> {
        let refresh_start = Utc::now();

        let address_strings = self.address_strings().await?;
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

    async fn update_utxo_set(
        &self,
        rpc_utxo_entries: Vec<RpcUtxosByAddressesEntry>,
        rpc_mempool_utxo_entries: Vec<RpcMempoolEntryByAddress>,
        refresh_start_time: DateTime<Utc>,
    ) -> Result<(), Box<dyn Error>> {
        let mut wallet_utxos: Vec<WalletUtxo> = vec![];

        let mut exculde = HashSet::new();
        for rpc_mempool_entries_by_address in rpc_mempool_utxo_entries {
            for rpc_mempool_entry in rpc_mempool_entries_by_address.sending {
                for input in rpc_mempool_entry.transaction.inputs {
                    exculde.insert(input.previous_outpoint);
                }
            }
        }

        let address_set = self.addresses.lock().await;
        let mut mempool_excluded_utxos: HashMap<WalletOutpoint, WalletUtxo> = HashMap::new();
        for rpc_utxo_entry in rpc_utxo_entries {
            let wallet_outpoint: WalletOutpoint = rpc_utxo_entry.outpoint.into();
            let wallet_utxo_entry: WalletUtxoEntry = rpc_utxo_entry.utxo_entry.into();

            let rpc_address = rpc_utxo_entry.address.unwrap();
            let address = address_set.get(&rpc_address.address_to_string()).unwrap();

            let wallet_utxo = WalletUtxo::new(wallet_outpoint, wallet_utxo_entry, address.clone());

            if exculde.contains(&rpc_utxo_entry.outpoint) {
                mempool_excluded_utxos.insert(wallet_utxo.outpoint.clone(), wallet_utxo);
            } else {
                wallet_utxos.push(wallet_utxo);
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

    async fn sync(&mut self) -> Result<(), Box<dyn Error>> {
        self.collect_far_addresses().await?;
        self.collect_recent_addresses().await?;
        self.refresh_utxos().await?;

        Ok(())
    }
}
