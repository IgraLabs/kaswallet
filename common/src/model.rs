use borsh::{BorshDeserialize, BorshSerialize};
use kaspa_addresses::Address;
use kaspa_bip32::DerivationPath;
use kaspa_consensus_core::sign::Signed;
use kaspa_consensus_core::sign::Signed::Partially;
use kaspa_consensus_core::tx::{
    ScriptPublicKey, SignableTransaction, TransactionOutpoint, UtxoEntry,
};
use kaspa_hashes::Hash;
use kaspa_rpc_core::{RpcTransactionOutpoint, RpcUtxoEntry};
use std::collections::HashSet;
use std::fmt::{Display, Formatter};

#[derive(Clone, Debug, Hash, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
#[borsh(use_discriminant = true)]
pub enum Keychain {
    External = 0,
    Internal = 1,
}

pub const KEYCHAINS: [Keychain; 2] = [Keychain::External, Keychain::Internal];

#[derive(Clone, Debug, Hash, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct WalletAddress {
    pub index: u32,
    pub cosigner_index: u16,
    pub keychain: Keychain,
}

impl WalletAddress {
    pub fn new(index: u32, cosigner_index: u16, keychain: Keychain) -> Self {
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

impl WalletOutpoint {
    pub fn new(transaction_id: Hash, index: u32) -> Self {
        Self {
            transaction_id,
            index,
        }
    }
}

impl Display for WalletOutpoint {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("({},{})", self.index, self.transaction_id))
    }
}

impl From<RpcTransactionOutpoint> for WalletOutpoint {
    fn from(value: RpcTransactionOutpoint) -> Self {
        Self {
            transaction_id: value.transaction_id,
            index: value.index,
        }
    }
}

impl From<TransactionOutpoint> for WalletOutpoint {
    fn from(value: TransactionOutpoint) -> Self {
        Self {
            transaction_id: value.transaction_id,
            index: value.index,
        }
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct WalletUtxoEntry {
    pub amount: u64,
    pub script_public_key: ScriptPublicKey,
    pub block_daa_score: u64,
    pub is_coinbase: bool,
}

impl WalletUtxoEntry {
    pub fn new(
        amount: u64,
        script_public_key: ScriptPublicKey,
        block_daa_score: u64,
        is_coinbase: bool,
    ) -> Self {
        Self {
            amount,
            script_public_key,
            block_daa_score,
            is_coinbase,
        }
    }
}

impl From<WalletUtxoEntry> for UtxoEntry {
    fn from(value: WalletUtxoEntry) -> UtxoEntry {
        UtxoEntry {
            amount: value.amount,
            script_public_key: value.script_public_key,
            block_daa_score: value.block_daa_score,
            is_coinbase: value.is_coinbase,
        }
    }
}

impl From<UtxoEntry> for WalletUtxoEntry {
    fn from(value: UtxoEntry) -> Self {
        Self {
            amount: value.amount,
            script_public_key: value.script_public_key,
            block_daa_score: value.block_daa_score,
            is_coinbase: value.is_coinbase,
        }
    }
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

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
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

pub struct WalletPayment {
    pub address: Address,
    pub amount: u64,
}
impl WalletPayment {
    pub fn new(address: Address, amount: u64) -> Self {
        Self { address, amount }
    }
}

#[derive(Debug, Clone, BorshSerialize, BorshDeserialize)]
pub struct WalletSignableTransaction {
    pub transaction: Signed,
    pub derivation_paths: HashSet<DerivationPath>,
    pub address_by_input_index: Vec<WalletAddress>,
    pub address_by_output_index: Vec<Address>,
}
impl WalletSignableTransaction {
    pub fn new(
        transaction: Signed,
        derivation_paths: HashSet<DerivationPath>,
        address_by_input_index: Vec<WalletAddress>,
        address_by_output_index: Vec<Address>,
    ) -> Self {
        Self {
            transaction,
            derivation_paths,
            address_by_input_index,
            address_by_output_index,
        }
    }

    pub fn new_from_unsigned(
        transaction: SignableTransaction,
        derivation_paths: HashSet<DerivationPath>,
        address_by_input_index: Vec<WalletAddress>,
        address_by_output_index: Vec<Address>,
    ) -> Self {
        Self {
            transaction: Partially(transaction),
            derivation_paths,
            address_by_input_index,
            address_by_output_index,
        }
    }
}
