use kaspa_consensus_core::tx::ScriptPublicKey;
use kaspa_hashes::Hash;

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

pub struct WalletOutpoint {
    pub transaction_id: Hash,
    pub index: u32,
}

pub struct WalletUtxoEntry {
    pub amount: u64,
    pub script_public_key: ScriptPublicKey,
    pub block_daa_score: u64,
    pub is_coinbase: bool,
}

pub struct WalletUtxo {
    outpoint: WalletOutpoint,
    utxo_entry: WalletUtxoEntry,
    address: WalletAddress,
}
