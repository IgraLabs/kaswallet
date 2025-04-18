use crate::address_manager::AddressManager;
use crate::model::{
    WalletAddress, WalletOutpoint, WalletSignableTransaction, WalletUtxo, WalletUtxoEntry,
};
use kaspa_consensus_core::config::params::Params;
use kaspa_wrpc_client::prelude::{
    GetBlockDagInfoResponse, RpcMempoolEntryByAddress, RpcUtxosByAddressesEntry,
};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct UtxoManager {
    address_manager: Arc<Mutex<AddressManager>>,
    mempool_excluded_utxos: HashMap<WalletOutpoint, WalletUtxo>,
    coinbase_maturity: u64, // Is different in testnet

    utxos_sorted_by_amount: Vec<WalletUtxo>,
    utxos_by_outpoint: HashMap<WalletOutpoint, WalletUtxo>,
}

impl UtxoManager {
    pub fn new(
        address_manager: Arc<Mutex<AddressManager>>,
        concensus_params: Params,
        block_dag_info: GetBlockDagInfoResponse,
    ) -> Self {
        let coinbase_maturity = concensus_params
            .coinbase_maturity()
            .get(block_dag_info.virtual_daa_score);

        Self {
            address_manager,
            mempool_excluded_utxos: Default::default(),
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

    pub async fn apply_transaction(&mut self, transaction: &WalletSignableTransaction) {
        let tx = &transaction.transaction.unwrap_ref().tx;

        for input in &tx.inputs {
            let outpoint = input.previous_outpoint;
            self.remove_utxo(&outpoint.into());
        }

        for (i, output) in tx.outputs.iter().enumerate() {
            let address = transaction.address_by_output_index[i].clone();
            let wallet_address: Option<WalletAddress>;
            {
                let address_manager = self.address_manager.lock().await;
                wallet_address = address_manager
                    .wallet_address_from_string(&address.to_string())
                    .await;
            }
            if wallet_address.is_none() {
                // this means payment is not to this wallet
                continue;
            }
            let wallet_address = wallet_address.unwrap();
            let outpoint = WalletOutpoint {
                transaction_id: tx.id(),
                index: i as u32,
            };
            let utxo = WalletUtxo::new(
                outpoint.clone(),
                WalletUtxoEntry {
                    amount: output.value,
                    script_public_key: output.script_public_key.clone(),
                    block_daa_score: 0,
                    is_coinbase: false,
                },
                wallet_address,
            );
            self.insert_utxo(outpoint, utxo);
        }
    }

    fn insert_utxo(&mut self, outpoint: WalletOutpoint, utxo: WalletUtxo) {
        self.utxos_by_outpoint.insert(outpoint, utxo.clone());
        let position = self
            .utxos_sorted_by_amount
            .binary_search_by(|existing_utxo| {
                existing_utxo.utxo_entry.amount.cmp(&utxo.utxo_entry.amount)
            })
            .unwrap_or_else(|e| e); // Use the insertion point if not found
        self.utxos_sorted_by_amount.insert(position, utxo);
    }

    fn remove_utxo(&mut self, outpoint: &WalletOutpoint) {
        let utxo = self.utxos_by_outpoint.remove(outpoint).unwrap();
        let position = self
            .utxos_sorted_by_amount
            .binary_search_by(|existing_utxo| {
                existing_utxo.utxo_entry.amount.cmp(&utxo.utxo_entry.amount)
            })
            .unwrap();
        self.utxos_sorted_by_amount.remove(position);
    }

    pub async fn update_utxo_set(
        &mut self,
        rpc_utxo_entries: Vec<RpcUtxosByAddressesEntry>,
        rpc_mempool_utxo_entries: Vec<RpcMempoolEntryByAddress>,
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

        Ok(())
    }

    fn update_utxos_sorted_by_amount(&mut self, mut wallet_utxos: Vec<WalletUtxo>) {
        wallet_utxos.sort_by(|a, b| a.utxo_entry.amount.cmp(&b.utxo_entry.amount));
        self.utxos_sorted_by_amount = wallet_utxos.clone();
    }

    fn update_utxos_by_outpoint(&mut self, wallet_utxos: Vec<WalletUtxo>) {
        self.utxos_by_outpoint.clear();
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
}
