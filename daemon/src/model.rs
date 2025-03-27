use borsh::{BorshDeserialize, BorshSerialize};
use kaspa_addresses::Address;
use kaspa_bip32::DerivationPath;
use kaspa_consensus_core::sign::Signed;
use kaspa_consensus_core::sign::Signed::Partially;
use kaspa_consensus_core::tx::{ScriptPublicKey, SignableTransaction, UtxoEntry};
use kaspa_hashes::Hash;
use kaspa_wrpc_client::prelude::{RpcTransactionOutpoint, RpcUtxoEntry};
use kaswallet_proto::kaswallet_proto::{
    Outpoint as ProtoOutpoint, ScriptPublicKey as ProtoScriptPublicKey, Utxo as ProtoUtxo,
    UtxoEntry as ProtoUtxoEntry,
};
use std::collections::HashSet;
use std::fmt;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum Keychain {
    External = 0,
    Internal = 1,
}

pub const KEYCHAINS: [Keychain; 2] = [Keychain::External, Keychain::Internal];

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
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

impl From<RpcTransactionOutpoint> for WalletOutpoint {
    fn from(value: RpcTransactionOutpoint) -> Self {
        Self {
            transaction_id: value.transaction_id,
            index: value.index,
        }
    }
}

impl Into<ProtoOutpoint> for WalletOutpoint {
    fn into(self) -> ProtoOutpoint {
        ProtoOutpoint {
            transaction_id: self.transaction_id.to_string(),
            index: self.index,
        }
    }
}

impl Into<WalletOutpoint> for ProtoOutpoint {
    fn into(self) -> WalletOutpoint {
        WalletOutpoint {
            transaction_id: Hash::from_str(&self.transaction_id).unwrap(),
            index: self.index,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WalletUtxoEntry {
    pub amount: u64,
    pub script_public_key: ScriptPublicKey,
    pub block_daa_score: u64,
    pub is_coinbase: bool,
}

impl Into<ProtoUtxoEntry> for WalletUtxoEntry {
    fn into(self) -> ProtoUtxoEntry {
        ProtoUtxoEntry {
            amount: self.amount,
            script_public_key: Some(ProtoScriptPublicKey {
                version: self.script_public_key.version as u32,
                script_public_key: hex::encode(self.script_public_key.script()),
            }),
            block_daa_score: self.block_daa_score,
            is_coinbase: self.is_coinbase,
        }
    }
}

impl Into<UtxoEntry> for WalletUtxoEntry {
    fn into(self) -> UtxoEntry {
        UtxoEntry {
            amount: self.amount,
            script_public_key: self.script_public_key,
            block_daa_score: self.block_daa_score,
            is_coinbase: self.is_coinbase,
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

#[derive(Debug, Clone)]
pub struct WalletUtxo {
    pub outpoint: WalletOutpoint,
    pub utxo_entry: WalletUtxoEntry,
    pub address: WalletAddress,
}

impl WalletUtxo {
    pub(crate) fn into_proto(self, is_pending: bool, is_dust: bool) -> ProtoUtxo {
        ProtoUtxo {
            outpoint: Some(self.outpoint.into()),
            utxo_entry: Some(self.utxo_entry.into()),
            is_pending,
            is_dust,
        }
    }
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

#[derive(Debug)]
pub struct UserInputError {
    pub message: String,
}

impl UserInputError {
    pub fn new(message: String) -> Self {
        UserInputError { message }
    }
}

impl Display for UserInputError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for UserInputError {}

#[derive(Debug, Clone, BorshSerialize, BorshDeserialize)]
pub struct WalletSignableTransaction {
    pub transaction: Signed,
    pub derivation_paths: HashSet<DerivationPath>,
}
impl WalletSignableTransaction {
    pub fn new(transaction: Signed, derivation_paths: HashSet<DerivationPath>) -> Self {
        Self {
            transaction,
            derivation_paths,
        }
    }

    pub fn new_from_unsigned(
        transaction: SignableTransaction,
        derivation_paths: HashSet<DerivationPath>,
    ) -> Self {
        Self {
            transaction: Partially(transaction),
            derivation_paths,
        }
    }
}
