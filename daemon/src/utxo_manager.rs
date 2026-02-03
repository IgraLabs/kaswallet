use crate::address_manager::AddressManager;
use common::model::{
    WalletAddress, WalletOutpoint, WalletSignableTransaction, WalletUtxo, WalletUtxoEntry,
};
use kaspa_addresses::Address;
use kaspa_consensus_core::config::params::Params;
use kaspa_rpc_core::{GetBlockDagInfoResponse, RpcMempoolEntryByAddress, RpcUtxosByAddressesEntry};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, Default)]
pub struct UtxoState {
    pub(crate) utxos_by_outpoint: HashMap<WalletOutpoint, WalletUtxo>,
    // Sorted by (amount, outpoint) so the outpoint can be used as a deterministic tiebreaker.
    pub(crate) utxo_keys_sorted_by_amount: Vec<(u64, WalletOutpoint)>,
}

impl UtxoState {
    pub fn new_empty() -> Self {
        Self::default()
    }

    pub fn utxo_count(&self) -> usize {
        self.utxos_by_outpoint.len()
    }

    pub fn utxos_by_outpoint(&self) -> &HashMap<WalletOutpoint, WalletUtxo> {
        &self.utxos_by_outpoint
    }

    pub fn get_utxo_by_outpoint(&self, outpoint: &WalletOutpoint) -> Option<&WalletUtxo> {
        self.utxos_by_outpoint.get(outpoint)
    }

    pub fn utxos_sorted_by_amount(&self) -> impl Iterator<Item = &WalletUtxo> + '_ {
        self.utxo_keys_sorted_by_amount.iter().map(|(_, outpoint)| {
            self.utxos_by_outpoint
                .get(outpoint)
                .expect("utxo_keys_sorted_by_amount contains unknown outpoint")
        })
    }
}

pub struct UtxoStateView {
    base_state: Arc<UtxoState>,
    removed_utxos: HashSet<WalletOutpoint>,
    added_utxos: HashMap<WalletOutpoint, WalletUtxo>,
    added_keys_sorted_by_amount: Vec<(u64, WalletOutpoint)>,
}

impl UtxoStateView {
    pub fn new(base_state: Arc<UtxoState>) -> Self {
        Self {
            base_state,
            removed_utxos: HashSet::new(),
            added_utxos: HashMap::new(),
            added_keys_sorted_by_amount: Vec::new(),
        }
    }

    pub fn from_mempool_overlay(
        base_state: Arc<UtxoState>,
        mempool_txs: &[WalletSignableTransaction],
        address_map: &HashMap<Address, WalletAddress>,
    ) -> Self {
        let mut removed_utxos = HashSet::new();
        let mut added_utxos = HashMap::new();
        let mut added_keys_sorted_by_amount = Vec::new();

        for tx in mempool_txs {
            let consensus_tx = &tx.transaction.unwrap_ref().tx;

            // Inputs are removed from the effective UTXO set.
            for input in &consensus_tx.inputs {
                removed_utxos.insert(input.previous_outpoint.into());
            }

            // Outputs are added if they pay to one of our addresses.
            for (i, output) in consensus_tx.outputs.iter().enumerate() {
                let kaspa_address = &tx.address_by_output_index[i];
                let Some(wallet_address) = address_map.get(kaspa_address) else {
                    continue;
                };

                let outpoint = WalletOutpoint {
                    transaction_id: consensus_tx.id(),
                    index: i as u32,
                };
                let utxo_entry = WalletUtxoEntry {
                    amount: output.value,
                    script_public_key: output.script_public_key.clone(),
                    block_daa_score: 0,
                    is_coinbase: false,
                };
                let utxo =
                    WalletUtxo::new(outpoint.clone(), utxo_entry, wallet_address.clone());

                let previous = added_utxos.insert(outpoint.clone(), utxo);
                debug_assert!(previous.is_none(), "mempool overlay inserted outpoint twice");
                added_keys_sorted_by_amount.push((output.value, outpoint));
            }
        }

        added_keys_sorted_by_amount.sort_unstable();

        Self {
            base_state,
            removed_utxos,
            added_utxos,
            added_keys_sorted_by_amount,
        }
    }

    pub fn base_state(&self) -> &Arc<UtxoState> {
        &self.base_state
    }

    pub fn utxo_count(&self) -> usize {
        let removed_in_base = self
            .removed_utxos
            .iter()
            .filter(|outpoint| self.base_state.utxos_by_outpoint.contains_key(*outpoint))
            .count();
        self.base_state.utxos_by_outpoint.len() - removed_in_base + self.added_utxos.len()
    }

    pub fn get_utxo_by_outpoint(&self, outpoint: &WalletOutpoint) -> Option<&WalletUtxo> {
        if self.removed_utxos.contains(outpoint) {
            return None;
        }
        if let Some(utxo) = self.added_utxos.get(outpoint) {
            return Some(utxo);
        }
        self.base_state.utxos_by_outpoint.get(outpoint)
    }

    pub fn utxos_iter(&self) -> impl Iterator<Item = &WalletUtxo> + '_ {
        let base_iter = self
            .base_state
            .utxos_by_outpoint
            .values()
            .filter(|utxo| !self.removed_utxos.contains(&utxo.outpoint));
        base_iter.chain(self.added_utxos.values())
    }

    pub fn utxos_sorted_by_amount(&self) -> UtxosSortedByAmountIter<'_> {
        UtxosSortedByAmountIter {
            view: self,
            base_index: 0,
            added_index: 0,
        }
    }
}

pub struct UtxosSortedByAmountIter<'a> {
    view: &'a UtxoStateView,
    base_index: usize,
    added_index: usize,
}

impl<'a> Iterator for UtxosSortedByAmountIter<'a> {
    type Item = &'a WalletUtxo;

    fn next(&mut self) -> Option<Self::Item> {
        let base_keys = self.view.base_state.utxo_keys_sorted_by_amount.as_slice();
        let added_keys = self.view.added_keys_sorted_by_amount.as_slice();

        loop {
            let next_base = loop {
                if self.base_index >= base_keys.len() {
                    break None;
                }
                let (amount, outpoint) = &base_keys[self.base_index];
                if self.view.removed_utxos.contains(outpoint) {
                    self.base_index += 1;
                    continue;
                }
                break Some((amount, outpoint));
            };

            let next_added = if self.added_index < added_keys.len() {
                let (amount, outpoint) = &added_keys[self.added_index];
                Some((amount, outpoint))
            } else {
                None
            };

            match (next_base, next_added) {
                (None, None) => return None,
                (Some((_amount, outpoint)), None) => {
                    self.base_index += 1;
                    return Some(
                        self.view
                            .base_state
                            .utxos_by_outpoint
                            .get(outpoint)
                            .expect("utxo_keys_sorted_by_amount contains unknown outpoint"),
                    );
                }
                (None, Some((_amount, outpoint))) => {
                    self.added_index += 1;
                    return Some(
                        self.view
                            .added_utxos
                            .get(outpoint)
                            .expect("added_keys_sorted_by_amount contains unknown outpoint"),
                    );
                }
                (Some((base_amount, base_outpoint)), Some((added_amount, added_outpoint))) => {
                    let base_key = (*base_amount, (*base_outpoint).clone());
                    let added_key = (*added_amount, (*added_outpoint).clone());
                    if base_key <= added_key {
                        self.base_index += 1;
                        return Some(
                            self.view
                                .base_state
                                .utxos_by_outpoint
                                .get(base_outpoint)
                                .expect("utxo_keys_sorted_by_amount contains unknown outpoint"),
                        );
                    }

                    self.added_index += 1;
                    return Some(
                        self.view
                            .added_utxos
                            .get(added_outpoint)
                            .expect("added_keys_sorted_by_amount contains unknown outpoint"),
                    );
                }
            }
        }
    }
}

pub struct UtxoManager {
    address_manager: Arc<Mutex<AddressManager>>,
    coinbase_maturity: u64, // Is different in testnet

    // Consensus snapshot (already includes node mempool effects from refresh).
    state: RwLock<Arc<UtxoState>>,

    // Wallet-generated, not-yet-accepted transactions. Applied as a lightweight overlay.
    // Stored separately because cloning the whole UTXO set per mempool tx is not viable at scale.
    mempool_transactions: Mutex<Vec<WalletSignableTransaction>>,
}

impl UtxoManager {
    pub fn new(
        address_manager: Arc<Mutex<AddressManager>>,
        consensus_params: Params,
        block_dag_info: GetBlockDagInfoResponse,
    ) -> Self {
        let coinbase_maturity = consensus_params
            .coinbase_maturity()
            .get(block_dag_info.virtual_daa_score);

        Self {
            address_manager,
            coinbase_maturity,
            state: RwLock::new(Arc::new(UtxoState::new_empty())),
            mempool_transactions: Mutex::new(Vec::new()),
        }
    }

    #[cfg(any(test, feature = "bench"))]
    pub fn new_for_bench(address_manager: Arc<Mutex<AddressManager>>) -> Self {
        Self {
            address_manager,
            coinbase_maturity: 0,
            state: RwLock::new(Arc::new(UtxoState::new_empty())),
            mempool_transactions: Mutex::new(Vec::new()),
        }
    }

    pub async fn state(&self) -> Arc<UtxoState> {
        let guard = self.state.read().await;
        Arc::clone(&*guard)
    }

    pub async fn state_with_mempool(&self) -> Result<UtxoStateView, Box<dyn Error + Send + Sync>> {
        let base_state = self.state().await;

        let mempool_txs = {
            let guard = self.mempool_transactions.lock().await;
            if guard.is_empty() {
                return Ok(UtxoStateView::new(base_state));
            }
            guard.clone()
        };

        // Map Address -> WalletAddress using the cached map (no per-output string parsing).
        let address_map: Arc<HashMap<Address, WalletAddress>> = {
            let address_manager = self.address_manager.lock().await;
            address_manager.monitored_address_map().await?
        };

        Ok(UtxoStateView::from_mempool_overlay(
            base_state,
            &mempool_txs,
            &address_map,
        ))
    }

    pub async fn update_utxo_set(
        &self,
        rpc_utxo_entries: Vec<RpcUtxosByAddressesEntry>,
        rpc_mempool_utxo_entries: Vec<RpcMempoolEntryByAddress>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut exclude: HashSet<WalletOutpoint> = HashSet::new();
        for rpc_mempool_entries_by_address in &rpc_mempool_utxo_entries {
            for sending_rpc_mempool_entry in &rpc_mempool_entries_by_address.sending {
                for input in &sending_rpc_mempool_entry.transaction.inputs {
                    exclude.insert(input.previous_outpoint.into());
                }
            }
        }

        let address_map: Arc<HashMap<Address, WalletAddress>> = {
            let address_manager = self.address_manager.lock().await;
            address_manager.monitored_address_map().await?
        };

        // Build new state without holding any UTXO locks.
        let mut utxos_by_outpoint: HashMap<WalletOutpoint, WalletUtxo> =
            HashMap::with_capacity(rpc_utxo_entries.len());
        let mut utxo_keys_sorted_by_amount: Vec<(u64, WalletOutpoint)> =
            Vec::with_capacity(rpc_utxo_entries.len());

        for rpc_utxo_entry in rpc_utxo_entries {
            let wallet_outpoint: WalletOutpoint = rpc_utxo_entry.outpoint.into();
            if exclude.contains(&wallet_outpoint) {
                continue;
            }

            let wallet_utxo_entry: WalletUtxoEntry = rpc_utxo_entry.utxo_entry.into();
            let amount = wallet_utxo_entry.amount;

            let Some(address) = rpc_utxo_entry.address else {
                continue;
            };
            let wallet_address = address_map
                .get(&address)
                .ok_or_else(|| {
                    format!(
                        "UTXO address {} not found in wallet address_set",
                        address.to_string()
                    )
                })?
                .clone();

            let wallet_utxo =
                WalletUtxo::new(wallet_outpoint.clone(), wallet_utxo_entry, wallet_address);

            let previous = utxos_by_outpoint.insert(wallet_outpoint.clone(), wallet_utxo);
            debug_assert!(previous.is_none(), "UTXO outpoint inserted twice");

            utxo_keys_sorted_by_amount.push((amount, wallet_outpoint));
        }

        for rpc_mempool_entry in rpc_mempool_utxo_entries {
            for receiving_rpc_mempool_entry in &rpc_mempool_entry.receiving {
                let transaction = &receiving_rpc_mempool_entry.transaction;
                let Some(transaction_verbose_data) = &transaction.verbose_data else {
                    panic!("transaction verbose data missing")
                };
                for (i, output) in transaction.outputs.iter().enumerate() {
                    let Some(output_verbose_data) = &output.verbose_data else {
                        panic!("output verbose data missing")
                    };
                    let address_string = output_verbose_data
                        .script_public_key_address
                        .address_to_string();
                    let address = Address::try_from(address_string.as_str()).map_err(|err| {
                        format!("invalid address in mempool output ({address_string}): {err}")
                    })?;
                    let Some(wallet_address) = address_map.get(&address) else {
                        // this means this output is not to this wallet
                        continue;
                    };

                    let wallet_outpoint =
                        WalletOutpoint::new(transaction_verbose_data.transaction_id, i as u32);

                    if exclude.contains(&wallet_outpoint) {
                        continue;
                    }
                    let utxo_entry = WalletUtxoEntry::new(
                        output.value,
                        output.script_public_key.clone(),
                        0,
                        false,
                    );

                    let utxo = WalletUtxo::new(
                        wallet_outpoint.clone(),
                        utxo_entry,
                        wallet_address.clone(),
                    );

                    let previous = utxos_by_outpoint.insert(wallet_outpoint.clone(), utxo);
                    debug_assert!(previous.is_none(), "mempool outpoint inserted twice");
                    utxo_keys_sorted_by_amount.push((output.value, wallet_outpoint));
                }
            }
        }

        utxo_keys_sorted_by_amount.sort_unstable();
        let new_state = Arc::new(UtxoState {
            utxos_by_outpoint,
            utxo_keys_sorted_by_amount,
        });

        // Swap the Arc pointer under a brief write lock.
        {
            let mut guard = self.state.write().await;
            *guard = new_state.clone();
        }

        self.prune_mempool_transactions_after_update(&new_state).await;
        Ok(())
    }

    pub async fn add_mempool_transaction(&self, transaction: &WalletSignableTransaction) {
        let mut mempool = self.mempool_transactions.lock().await;
        mempool.push(transaction.clone());
    }

    async fn prune_mempool_transactions_after_update(&self, new_state: &UtxoState) {
        let mut mempool = self.mempool_transactions.lock().await;
        mempool.retain(|transaction| {
            for input in transaction.transaction.unwrap_ref().tx.inputs.iter() {
                let outpoint = input.previous_outpoint;
                if !new_state.utxos_by_outpoint.contains_key(&outpoint.into()) {
                    // Transaction is either accepted (now covered by RPC mempool snapshot) or double-spent.
                    return false;
                }
            }
            true
        });
    }

    pub fn is_utxo_pending(&self, utxo: &WalletUtxo, virtual_daa_score: u64) -> bool {
        if !utxo.utxo_entry.is_coinbase {
            return false;
        }

        utxo.utxo_entry.block_daa_score + self.coinbase_maturity > virtual_daa_score
    }
}

#[cfg(test)]
mod tests {
    use super::UtxoManager;
    use crate::address_manager::AddressManager;
    use common::keys::Keys;
    use common::model::{Keychain, WalletAddress, WalletOutpoint, WalletSignableTransaction};
    use kaspa_addresses::{Address, Prefix, Version};
    use kaspa_bip32::Prefix as XPubPrefix;
    use kaspa_consensus_core::tx::{
        ScriptPublicKey, SignableTransaction, Transaction, TransactionInput, TransactionOutpoint,
        TransactionOutput, UtxoEntry,
    };
    use kaspa_hashes::Hash;
    use kaspa_rpc_core::{RpcTransactionOutpoint, RpcUtxoEntry, RpcUtxosByAddressesEntry};
    use std::collections::HashSet;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn txid(i: u32) -> Hash {
        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&i.to_le_bytes());
        Hash::from_bytes(bytes)
    }

    fn make_address(prefix: Prefix, i: u32) -> Address {
        let mut payload = [0u8; 32];
        payload[..4].copy_from_slice(&i.to_le_bytes());
        Address::new(prefix, Version::PubKey, &payload)
    }

    fn make_outpoint(i: u32) -> RpcTransactionOutpoint {
        RpcTransactionOutpoint {
            transaction_id: txid(i),
            index: i,
        }
    }

    fn make_rpc_utxo_entry(amount: u64) -> RpcUtxoEntry {
        RpcUtxoEntry::new(amount, ScriptPublicKey::from_vec(0, vec![]), 0, false)
    }

    fn make_rpc_utxo(i: u32, address: Address) -> RpcUtxosByAddressesEntry {
        let amount = ((i % 10_000) + 1) as u64;
        RpcUtxosByAddressesEntry {
            address: Some(address),
            outpoint: make_outpoint(i),
            utxo_entry: make_rpc_utxo_entry(amount),
        }
    }

    #[tokio::test]
    async fn update_utxo_set_produces_sorted_index() {
        let keys = Arc::new(Keys::new(
            "unused".to_string(),
            1,
            vec![],
            XPubPrefix::XPUB,
            vec![],
            0,
            0,
            1,
            0,
        ));
        let address_manager = AddressManager::new(keys, Prefix::Mainnet);
        let address = make_address(Prefix::Mainnet, 1);
        let wa = WalletAddress::new(1, 0, Keychain::External);
        address_manager.insert_address_for_bench(address.clone(), wa).await;

        let address_manager = Arc::new(Mutex::new(address_manager));
        let utxo_manager = UtxoManager::new_for_bench(address_manager);

        let entries = vec![
            make_rpc_utxo(1, address.clone()), // amount 2
            make_rpc_utxo(2, address.clone()), // amount 3
            make_rpc_utxo(10_000, address.clone()), // amount 1
        ];

        utxo_manager.update_utxo_set(entries, vec![]).await.unwrap();

        let state = utxo_manager.state().await;
        let amounts: Vec<u64> = state
            .utxos_sorted_by_amount()
            .map(|utxo| utxo.utxo_entry.amount)
            .collect();
        assert_eq!(amounts, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn state_snapshots_remain_valid_after_update() {
        let keys = Arc::new(Keys::new(
            "unused".to_string(),
            1,
            vec![],
            XPubPrefix::XPUB,
            vec![],
            0,
            0,
            1,
            0,
        ));
        let address_manager = AddressManager::new(keys, Prefix::Mainnet);
        let address = make_address(Prefix::Mainnet, 1);
        let wa = WalletAddress::new(1, 0, Keychain::External);
        address_manager.insert_address_for_bench(address.clone(), wa).await;

        let address_manager = Arc::new(Mutex::new(address_manager));
        let utxo_manager = UtxoManager::new_for_bench(address_manager);

        utxo_manager
            .update_utxo_set(vec![make_rpc_utxo(1, address.clone())], vec![])
            .await
            .unwrap();
        let old_state = utxo_manager.state().await;
        assert_eq!(old_state.utxo_count(), 1);

        utxo_manager
            .update_utxo_set(
                vec![make_rpc_utxo(1, address.clone()), make_rpc_utxo(2, address.clone())],
                vec![],
            )
            .await
            .unwrap();
        let new_state = utxo_manager.state().await;
        assert_eq!(new_state.utxo_count(), 2);

        // Old snapshot remains valid and unchanged.
        assert_eq!(old_state.utxo_count(), 1);
    }

    #[tokio::test]
    async fn state_with_mempool_overlays_wallet_transactions() {
        let keys = Arc::new(Keys::new(
            "unused".to_string(),
            1,
            vec![],
            XPubPrefix::XPUB,
            vec![],
            0,
            0,
            1,
            0,
        ));
        let address_manager = AddressManager::new(keys, Prefix::Mainnet);
        let address = make_address(Prefix::Mainnet, 1);
        let wa = WalletAddress::new(1, 0, Keychain::External);
        address_manager.insert_address_for_bench(address.clone(), wa.clone()).await;

        let address_manager = Arc::new(Mutex::new(address_manager));
        let utxo_manager = UtxoManager::new_for_bench(address_manager);

        // Base state: one confirmed UTXO.
        utxo_manager
            .update_utxo_set(vec![make_rpc_utxo(1, address.clone())], vec![])
            .await
            .unwrap();
        let base_state = utxo_manager.state().await;
        let base_outpoint: WalletOutpoint = make_outpoint(1).into();
        let base_utxo = base_state
            .get_utxo_by_outpoint(&base_outpoint)
            .expect("base utxo missing");

        // Local wallet tx spends that UTXO and creates one output to our address.
        let input = TransactionInput::new(
            TransactionOutpoint::new(base_outpoint.transaction_id, base_outpoint.index),
            vec![],
            0,
            1,
        );
        let output = TransactionOutput::new(1, ScriptPublicKey::from_vec(0, vec![]));
        let tx = Transaction::new(0, vec![input], vec![output], 0, Default::default(), 0, vec![]);
        let input_entry: UtxoEntry = base_utxo.utxo_entry.clone().into();
        let signable = SignableTransaction::with_entries(tx, vec![input_entry]);
        let wallet_tx = WalletSignableTransaction::new_from_unsigned(
            signable,
            HashSet::new(),
            vec![wa],
            vec![address.clone()],
        );

        utxo_manager.add_mempool_transaction(&wallet_tx).await;

        let view = utxo_manager.state_with_mempool().await.unwrap();

        // View hides the spent outpoint but base snapshot remains unchanged.
        assert!(view.get_utxo_by_outpoint(&base_outpoint).is_none());
        assert!(base_state.get_utxo_by_outpoint(&base_outpoint).is_some());

        // View includes the newly created outpoint.
        let created_outpoint = WalletOutpoint {
            transaction_id: wallet_tx.transaction.unwrap_ref().tx.id(),
            index: 0,
        };
        assert!(view.get_utxo_by_outpoint(&created_outpoint).is_some());
        assert!(base_state.get_utxo_by_outpoint(&created_outpoint).is_none());
    }
}
