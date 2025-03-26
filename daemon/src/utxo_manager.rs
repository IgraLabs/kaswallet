use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::sync::Arc;
use chrono::{DateTime, Utc};
use kaspa_consensus_core::constants::SOMPI_PER_KASPA;
use kaspa_wrpc_client::KaspaRpcClient;
use kaspa_wrpc_client::prelude::{RpcApi, RpcMempoolEntryByAddress, RpcUtxosByAddressesEntry};
use tokio::sync::Mutex;
use crate::address_manager::AddressManager;
use crate::model::{UserInputError, WalletAddress, WalletOutpoint, WalletUtxo, WalletUtxoEntry};

pub struct UtxoManager{
    kaspa_rpc_client: Arc<KaspaRpcClient>,
    address_manager: Mutex<Arc<AddressManager>>,
    utxos_sorted_by_amount: Vec<WalletUtxo>,
    mempool_excluded_utxos: HashMap<WalletOutpoint, WalletUtxo>,
    used_outpoints: HashMap<WalletOutpoint, DateTime<Utc>>,
    coinbase_maturity: u64, // Is different in testnet
}

// The minimal change amount to target in order to avoid large storage mass (see KIP9 for more details).
// By having at least 10KAS in the change output we make sure that the storage mass charged for change is
// at most 1000 gram. Generally, if the payment is above 10KAS as well, the resulting storage mass will be
// in the order of magnitude of compute mass and wil not incur additional charges.
// Additionally, every transaction with send value > ~0.1 KAS should succeed (at most ~99K storage mass for payment
// output, thus overall lower than standard mass upper bound which is 100K gram)
const MIN_CHANGE_TARGET: u64 = SOMPI_PER_KASPA * 10;

impl UtxoManager {
    pub fn new(kaspa_rpc_client: Arc<KaspaRpcClient>, address_manager: Mutex<Arc<AddressManager>>, coinbase_maturity: u64) -> Self {
        Self{
            kaspa_rpc_client,
            address_manager,
            utxos_sorted_by_amount: Vec::new(),
            mempool_excluded_utxos: Default::default(),
            used_outpoints: HashMap::new(),
            coinbase_maturity,
        }
    }

    pub async fn get_utxos_sorted_by_amount(&self) -> Vec<WalletUtxo> {
        self.utxos_sorted_by_amount.clone()
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

        wallet_utxos.sort_by(|a, b| a.utxo_entry.amount.cmp(&b.utxo_entry.amount));
        self.utxos_sorted_by_amount= wallet_utxos;
        self.mempool_excluded_utxos= mempool_excluded_utxos;

        // Cleanup expired used outpoints to avoid a memory leak
        for (outpoint, broadcast_time) in self.used_outpoints {
            if self.has_used_outpoint_expired(&broadcast_time).await {
                self.used_outpoints.remove(&outpoint);
            }
        }
        Ok(())
    }

    async fn select_utxos(
        &self,
        preselected_utxos: HashMap<WalletOutpoint, WalletUtxo>,
        allowed_used_outpoints: HashSet<WalletOutpoint>,
        amount: u64,
        is_send_all: bool,
        fee_rate: f64,
        max_fee: u64,
        from_addresses: Vec<&WalletAddress>,
        payload: &Vec<u8>,
    ) -> Result<(Vec<WalletUtxo>, u64, u64), Box<dyn Error + Send + Sync>> {
        let mut total_value = 0;
        let mut selected_utxos = vec![];

        let dag_info = self.kaspa_rpc_client.get_block_dag_info().await?;

        let mut utxo_manager = self;
        let mut fee = 0;
        let mut iteration = async |utxo: &WalletUtxo,
                                   avoid_preselected: bool|
                                   -> Result<bool, Box<dyn Error + Send + Sync>> {
            if !from_addresses.is_empty() && !from_addresses.contains(&&utxo.address) {
                return Ok(true);
            }
            if utxo_manager.is_utxo_pending(utxo, dag_info.virtual_daa_score) {
                return Ok(true);
            }

            {
                let mut used_outpoints = utxo_manager.used_outpoints.clone();
                if let Some(broadcast_time) = used_outpoints.get(&utxo.outpoint) {
                    if allowed_used_outpoints.contains(&utxo.outpoint) {
                        if utxo_manager.has_used_outpoint_expired(broadcast_time).await {
                            used_outpoints.remove(&utxo.outpoint);
                        }
                    } else {
                        return Ok(true);
                    }
                }
                utxo_manager.used_outpoints = used_outpoints;
            }

            if avoid_preselected {
                if preselected_utxos.contains_key(&utxo.outpoint) {
                    return Ok(true);
                }
            }

            selected_utxos.push(utxo.clone());
            total_value += utxo.utxo_entry.amount;
            let estimated_recipient_value = if is_send_all { total_value } else { amount };
            fee = self
                .estimate_fee(
                    &selected_utxos,
                    fee_rate,
                    max_fee,
                    estimated_recipient_value,
                    payload,
                )
                .await?;

            let total_spend = amount + fee;
            // Two break cases (if not send all):
            // 		1. total_value == totalSpend, so there's no change needed -> number of outputs = 1, so a single input is sufficient
            // 		2. total_value > totalSpend, so there will be change and 2 outputs, therefor in order to not struggle with --
            //		   2.1 go-nodes dust patch we try and find at least 2 inputs (even though the next one is not necessary in terms of spend value)
            // 		   2.2 KIP9 we try and make sure that the change amount is not too small
            if is_send_all {
                return Ok(true);
            }
            if total_value == total_spend {
                return Ok(false);
            }
            if total_value >= total_spend + MIN_CHANGE_TARGET && selected_utxos.len() > 1 {
                return Ok(false);
            }
            return Ok(true);
        };

        let mut should_continue = true;
        for (_, preselected_utxo) in &preselected_utxos {
            should_continue = iteration(preselected_utxo, false).await?;
            if !should_continue {
                break;
            };
        }
        if should_continue {
            {
                for utxo in self.utxos_sorted_by_amount.iter() {
                    should_continue = iteration(&utxo, false).await?;
                    if !should_continue {
                        break;
                    }
                }
            }
        }

        let total_spend: u64;
        let total_received: u64;
        if is_send_all {
            total_spend = total_value;
            total_received = total_value - fee;
        } else {
            total_spend = amount + fee;
            total_received = amount;
        }

        if total_value < total_spend {
            return Err(Box::new(UserInputError::new(format!(
                "Insufficient funds for send: {} required, while only {} available",
                amount / SOMPI_PER_KASPA,
                total_value / SOMPI_PER_KASPA
            ))));
        }

        Ok((selected_utxos, total_received, total_value - total_spend))
    }

    pub fn is_utxo_pending(&self, utxo: &WalletUtxo, virtual_daa_score: u64) -> bool {
        if !utxo.utxo_entry.is_coinbase {
            return false;
        }

        utxo.utxo_entry.block_daa_score + self.coinbase_maturity > virtual_daa_score
    }
}