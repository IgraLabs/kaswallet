use borsh::{BorshDeserialize, BorshSerialize};
use kaspa_addresses::Address;
use kaspa_bip32::DerivationPath;
use kaspa_consensus_core::sign::Signed;
use kaspa_consensus_core::tx::{
    ScriptPublicKey, SignableTransaction, TransactionOutpoint, UtxoEntry,
};
use kaspa_hashes::Hash;
use kaspa_rpc_core::{RpcTransactionOutpoint, RpcUtxoEntry};
use std::fmt::{Display, Formatter};

/// Wallet-side mirror of `kaspa_consensus_core::sign::Signed`.
///
/// Upstream removed Clone/Debug/Borsh derives from the consensus `Signed`
/// enum on the Toccata branch, but the wallet's transaction pipeline still
/// needs to clone and inspect signed transactions (e.g. when reissuing a
/// partially-signed payload to a co-signer). This wrapper restores those
/// traits and gives us a stable type to hang wallet-specific methods off.
/// Conversion to/from the upstream `Signed` is zero-cost.
#[derive(Debug, Clone)]
pub enum WalletSigned {
    Fully(SignableTransaction),
    Partially(SignableTransaction),
}

impl WalletSigned {
    /// Consume the wrapper and return the inner `SignableTransaction`,
    /// discarding the signed-ness discriminant. Infallible.
    pub fn into_inner(self) -> SignableTransaction {
        match self {
            Self::Fully(tx) | Self::Partially(tx) => tx,
        }
    }

    /// Borrow the inner `SignableTransaction` regardless of signed-ness.
    /// Use only for read-only inspection (mass calc, ids) that does not
    /// care whether the transaction has been fully signed.
    pub fn inner(&self) -> &SignableTransaction {
        match self {
            Self::Fully(tx) | Self::Partially(tx) => tx,
        }
    }
}

impl From<Signed> for WalletSigned {
    fn from(value: Signed) -> Self {
        match value {
            Signed::Fully(tx) => Self::Fully(tx),
            Signed::Partially(tx) => Self::Partially(tx),
        }
    }
}

impl From<WalletSigned> for Signed {
    fn from(value: WalletSigned) -> Self {
        match value {
            WalletSigned::Fully(tx) => Self::Fully(tx),
            WalletSigned::Partially(tx) => Self::Partially(tx),
        }
    }
}

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
    // True for UTXOs ingested from kaspad's mempool-receiving view
    // (`get_mempool_entries_by_addresses`). These outputs only exist in
    // kaspad's mempool — they can disappear (eviction, re-org, double-spend)
    // before we get a chance to spend them, in which case kaspad rejects
    // the child as Orphan. `select_utxos` skips entries with this flag
    // set so we only ever spend confirmed UTXOs by default.
    //
    // Stays `false` for: all confirmed on-chain UTXOs from
    // `get_utxos_by_addresses`, and wallet-self-tracking outputs added
    // via `apply_mempool_transaction` (those have their own safe replay
    // path through `apply_mempool_transactions_after_update`).
    pub is_unconfirmed: bool,
}

impl WalletUtxoEntry {
    pub fn new(
        amount: u64,
        script_public_key: ScriptPublicKey,
        block_daa_score: u64,
        is_coinbase: bool,
        is_unconfirmed: bool,
    ) -> Self {
        Self {
            amount,
            script_public_key,
            block_daa_score,
            is_coinbase,
            is_unconfirmed,
        }
    }
}

impl From<WalletUtxoEntry> for UtxoEntry {
    fn from(value: WalletUtxoEntry) -> UtxoEntry {
        // Wallet UTXOs never originate from a covenant-bearing output, so
        // covenant_id stays None on the round-trip into upstream UtxoEntry.
        // `is_unconfirmed` is a wallet-internal selection hint; upstream
        // has no equivalent and it would have no meaning at the consensus
        // boundary, so it drops here intentionally.
        UtxoEntry {
            amount: value.amount,
            script_public_key: value.script_public_key,
            block_daa_score: value.block_daa_score,
            is_coinbase: value.is_coinbase,
            covenant_id: None,
        }
    }
}

// `From<UtxoEntry>` is intentionally kept infallible: every `UtxoEntry`
// constructed inside this crate (proto round-trip, mempool replay) sets
// `covenant_id: None`. The external risk vector — kaspad RPC supplying a
// covenant-bound entry — flows through `RpcUtxoEntry` and is filtered at
// the sync boundary in `UtxoManager::update_utxo_set`.
//
// `is_unconfirmed` defaults to `false`. The two callers — proto round-trip
// in service handlers and mempool replay in `apply_mempool_transactions_after_update`
// — both operate on transactions whose ownership has already been
// established and which are safe to chain. The genuinely ephemeral path
// (kaspad mempool-receiving) constructs `WalletUtxoEntry` directly and
// sets the flag explicitly.
impl From<UtxoEntry> for WalletUtxoEntry {
    fn from(value: UtxoEntry) -> Self {
        Self {
            amount: value.amount,
            script_public_key: value.script_public_key,
            block_daa_score: value.block_daa_score,
            is_coinbase: value.is_coinbase,
            is_unconfirmed: false,
        }
    }
}

/// Error returned when an upstream UTXO entry cannot be represented as a
/// `WalletUtxoEntry`. Today this only fires for covenant-bound entries (which
/// the wallet cannot spend); future variants can extend the type.
#[derive(Debug, thiserror::Error)]
pub enum WalletUtxoEntryError {
    #[error("UTXO is covenant-bound (covenant_id={covenant_id:?}); wallet cannot spend covenants")]
    CovenantBound { covenant_id: Hash },
}

impl TryFrom<RpcUtxoEntry> for WalletUtxoEntry {
    type Error = WalletUtxoEntryError;

    fn try_from(value: RpcUtxoEntry) -> Result<Self, Self::Error> {
        // The wallet does not support spending covenant-bound UTXOs.
        // The fallible conversion forces every call site to handle (or
        // explicitly skip) covenant-bound entries; the runtime gate in
        // `UtxoManager::update_utxo_set` filters them with a warn log
        // before reaching here.
        //
        // `RpcUtxoEntry` reaches us only through `get_utxos_by_addresses`,
        // which surfaces confirmed (block-included) UTXOs — kaspad's
        // mempool-receiving outputs travel through a different RPC and
        // are constructed elsewhere. So `is_unconfirmed = false` is the
        // correct default for this path.
        if let Some(covenant_id) = value.covenant_id {
            return Err(WalletUtxoEntryError::CovenantBound { covenant_id });
        }
        Ok(Self {
            amount: value.amount,
            script_public_key: value.script_public_key,
            block_daa_score: value.block_daa_score,
            is_coinbase: value.is_coinbase,
            is_unconfirmed: false,
        })
    }
}

/// True if an `RpcUtxoEntry` is spendable by this wallet — i.e. carries
/// no covenant binding. Use this to filter at the kaspad RPC boundary
/// (the only path covenant-bound entries can reach the wallet) before
/// the fallible `TryFrom<RpcUtxoEntry> for WalletUtxoEntry` conversion.
pub fn is_spendable_rpc_utxo_entry(entry: &RpcUtxoEntry) -> bool {
    entry.covenant_id.is_none()
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

// `transaction` is held in the wallet-side `WalletSigned` wrapper so the
// struct stays Clone/Debug across the Toccata-era trait changes. Borsh
// serialization is unused in this codebase (gRPC transport uses prost via
// proto_convert) — derives intentionally omitted.
#[derive(Debug, Clone)]
pub struct WalletSignableTransaction {
    pub transaction: WalletSigned,
    pub derivation_paths: Vec<DerivationPath>,
    pub address_by_input_index: Vec<WalletAddress>,
    pub address_by_output_index: Vec<Address>,
}
impl WalletSignableTransaction {
    pub fn new(
        transaction: WalletSigned,
        derivation_paths: Vec<DerivationPath>,
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
        derivation_paths: Vec<DerivationPath>,
        address_by_input_index: Vec<WalletAddress>,
        address_by_output_index: Vec<Address>,
    ) -> Self {
        Self {
            transaction: WalletSigned::Partially(transaction),
            derivation_paths,
            address_by_input_index,
            address_by_output_index,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_spk() -> ScriptPublicKey {
        ScriptPublicKey::new(0, vec![].into())
    }

    #[test]
    fn rpc_utxo_entry_try_from_marks_confirmed() {
        // `RpcUtxoEntry` only reaches the wallet via `get_utxos_by_addresses`,
        // which yields confirmed (block-included) UTXOs. The conversion
        // must reflect that — `select_utxos` relies on this default.
        let rpc_entry = RpcUtxoEntry {
            amount: 100,
            script_public_key: empty_spk(),
            block_daa_score: 42,
            is_coinbase: false,
            covenant_id: None,
        };
        let wallet_entry = WalletUtxoEntry::try_from(rpc_entry).unwrap();
        assert!(!wallet_entry.is_unconfirmed);
        assert_eq!(wallet_entry.block_daa_score, 42);
    }

    #[test]
    fn utxo_entry_from_marks_confirmed() {
        // Same default for the infallible `UtxoEntry` conversion. The only
        // callers (proto round-trip, mempool replay) operate on payloads
        // already known to be safe to chain.
        let upstream_entry = UtxoEntry {
            amount: 100,
            script_public_key: empty_spk(),
            block_daa_score: 7,
            is_coinbase: true,
            covenant_id: None,
        };
        let wallet_entry: WalletUtxoEntry = upstream_entry.into();
        assert!(!wallet_entry.is_unconfirmed);
        assert!(wallet_entry.is_coinbase);
    }

    #[test]
    fn wallet_entry_to_utxo_entry_drops_unconfirmed_bit() {
        // The unconfirmed flag is wallet-internal. Upstream `UtxoEntry`
        // does not represent it and must not — it would be meaningless at
        // the consensus boundary. The conversion drops it silently.
        let wallet_entry = WalletUtxoEntry::new(100, empty_spk(), 0, false, true);
        let upstream: UtxoEntry = wallet_entry.into();
        // No `is_unconfirmed` field to assert against; this test exists to
        // catch a future regression where someone tries to surface it
        // upstream and breaks the boundary contract.
        assert_eq!(upstream.amount, 100);
        assert_eq!(upstream.covenant_id, None);
    }
}
