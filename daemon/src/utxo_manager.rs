use crate::address_manager::AddressManager;
use crate::model::{WalletOutpoint, WalletUtxo, WalletUtxoEntry};
use chrono::{DateTime, Utc};
use kaspa_wallet_core::utxo::NetworkParams;
use kaspa_wrpc_client::prelude::{RpcMempoolEntryByAddress, RpcUtxosByAddressesEntry};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct UtxoManager {
    address_manager: Arc<Mutex<AddressManager>>,
    mempool_excluded_utxos: HashMap<WalletOutpoint, WalletUtxo>,
    start_time_of_last_completed_refresh: DateTime<Utc>,
    coinbase_maturity: u64, // Is different in testnet

    utxos_sorted_by_amount: Vec<WalletUtxo>,
    utxos_by_outpoint: HashMap<WalletOutpoint, WalletUtxo>,
}

impl UtxoManager {
    pub fn new(
        address_manager: Arc<Mutex<AddressManager>>,
        network_params: &NetworkParams,
    ) -> Self {
        let coinbase_maturity = network_params
            .coinbase_transaction_maturity_period_daa
            .load(Relaxed);

        Self {
            address_manager,
            mempool_excluded_utxos: Default::default(),
            start_time_of_last_completed_refresh: DateTime::<Utc>::MIN_UTC,
            coinbase_maturity,
            utxos_sorted_by_amount: Vec::new(),
            utxos_by_outpoint: Default::default(),
        }
    }

    pub fn utxos_sorted_by_amount(&self) -> &Vec<WalletUtxo> {
        &self.utxos_sorted_by_amount
    }

    pub fn utxos_by_outpoint(&self) -> &HashMap<WalletOutpoint, WalletUtxo> {
        &self.utxos_by_outpoint
    }

    pub async fn update_utxo_set(
        &mut self,
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

        self.update_utxos_sorted_by_amount(wallet_utxos.clone());
        self.update_utxos_by_outpoint(wallet_utxos);

        self.mempool_excluded_utxos = mempool_excluded_utxos;

        self.start_time_of_last_completed_refresh = refresh_start_time;
        Ok(())
    }

    fn update_utxos_sorted_by_amount(&mut self, mut wallet_utxos: Vec<WalletUtxo>) {
        wallet_utxos.sort_by(|a, b| a.utxo_entry.amount.cmp(&b.utxo_entry.amount));
        self.utxos_sorted_by_amount = wallet_utxos.clone();
    }

    fn update_utxos_by_outpoint(&mut self, wallet_utxos: Vec<WalletUtxo>) {
        for wallet_utxo in wallet_utxos {
            self.utxos_by_outpoint
                .insert(wallet_utxo.outpoint.clone(), wallet_utxo);
        }
    }

    pub fn is_utxo_pending(&self, utxo: &WalletUtxo, virtual_daa_score: u64) -> bool {
        if !utxo.utxo_entry.is_coinbase {
            return false;
        }

        utxo.utxo_entry.block_daa_score + self.coinbase_maturity > virtual_daa_score
    }

    pub fn start_time_of_last_completed_refresh(&self) -> DateTime<Utc> {
        self.start_time_of_last_completed_refresh
    }
}
