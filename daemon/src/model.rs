use kaspa_consensus_core::tx::ScriptPublicKey;
use kaspa_hashes::Hash;
use kaspa_wrpc_client::prelude::{RpcTransactionOutpoint, RpcUtxoEntry};

#[derive(Clone, Debug, PartialEq)]
pub enum Keychain {
    External = 0,
    Internal = 1,
}

pub const KEYCHAINS: [Keychain; 2] = [Keychain::External, Keychain::Internal];

#[derive(Clone, Debug)]
pub struct WalletAddress {
    pub index: u32,
    pub cosigner_index: u32,
    pub keychain: Keychain,
}

impl WalletAddress {
    pub fn new(index: u32, cosigner_index: u32, keychain: Keychain) -> Self {
        WalletAddress {
            index,
            cosigner_index,
            keychain,
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct WalletOutpoint {
    pub transaction_id: Hash,
    pub index: u32,
}

impl From<RpcTransactionOutpoint> for WalletOutpoint {
    fn from(value: RpcTransactionOutpoint) -> Self {
        Self {
            transaction_id: value.transaction_id,
            index: value.index,
        }
    }
}

#[derive(Debug)]
pub struct WalletUtxoEntry {
    pub amount: u64,
    pub script_public_key: ScriptPublicKey,
    pub block_daa_score: u64,
    pub is_coinbase: bool,
}

impl From<RpcUtxoEntry> for WalletUtxoEntry {
    fn from(value: RpcUtxoEntry) -> Self {
        Self {
            amount: value.amount,
            script_public_key: value.script_public_key,
            block_daa_score: value.block_daa_score,
            is_coinbase: value.is_coinbase,
        }
    }
}

#[derive(Debug)]
pub struct WalletUtxo {
    pub outpoint: WalletOutpoint,
    pub utxo_entry: WalletUtxoEntry,
    pub address: WalletAddress,
}

impl WalletUtxo {
    pub fn new(
        outpoint: WalletOutpoint,
        utxo_entry: WalletUtxoEntry,
        address: WalletAddress,
    ) -> Self {
        Self {
            outpoint,
            utxo_entry,
            address,
        }
    }
}
