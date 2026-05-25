use crate::error_location::ErrorLocation;
use crate::errors::{UserInputError, WalletError, WalletResult};
use crate::model::{
    Keychain, WalletAddress, WalletOutpoint, WalletSignableTransaction, WalletUtxo, WalletUtxoEntry,
};
use kaspa_addresses::Address;
use kaspa_bip32::{ChildNumber, DerivationPath};
use kaspa_consensus_core::sign::Signed;
use kaspa_consensus_core::subnets::{SUBNETWORK_ID_SIZE, SubnetworkId};
use kaspa_consensus_core::tx::{
    ScriptPublicKey, SignableTransaction, Transaction, TransactionInput, TransactionOutpoint,
    TransactionOutput, TxInputMass, UtxoEntry,
};
use kaspa_hashes::Hash;
use proto::kaswallet_proto::{
    DerivationPath as ProtoDerivationPath, Keychain as ProtoKeychain,
    NonContextualMasses as ProtoNonContextualMasses, OptionalUtxoEntry as ProtoOptionalUtxoEntry,
    Outpoint as ProtoOutpoint, ScriptPublicKey as ProtoScriptPublicKey,
    SignableTransaction as ProtoSignableTransaction, SignedTransaction as ProtoSignedTransaction,
    Transaction as ProtoTransaction, TransactionInput as ProtoTransactionInput,
    TransactionOutpoint as ProtoTransactionOutpoint, TransactionOutput as ProtoTransactionOutput,
    Utxo as ProtoUtxo, UtxoEntry as ProtoUtxoEntry, WalletAddress as ProtoWalletAddress,
    WalletSignableTransaction as ProtoWalletSignableTransaction, signed_transaction,
};
use std::str::FromStr;

pub fn derivation_path_to_proto(value: DerivationPath) -> ProtoDerivationPath {
    ProtoDerivationPath {
        path: value
            .as_ref()
            .iter()
            .map(|child_number| child_number.0)
            .collect(),
    }
}

pub fn derivation_path_from_proto(value: ProtoDerivationPath) -> DerivationPath {
    let mut derivation_path = DerivationPath::default();
    for child_number_value in value.path {
        derivation_path.push(ChildNumber(child_number_value));
    }
    derivation_path
}

impl From<Keychain> for ProtoKeychain {
    fn from(value: Keychain) -> Self {
        match value {
            Keychain::External => ProtoKeychain::External,
            Keychain::Internal => ProtoKeychain::Internal,
        }
    }
}

impl From<ProtoKeychain> for Keychain {
    fn from(value: ProtoKeychain) -> Self {
        match value {
            ProtoKeychain::External => Keychain::External,
            ProtoKeychain::Internal => Keychain::Internal,
        }
    }
}

impl From<WalletAddress> for ProtoWalletAddress {
    fn from(value: WalletAddress) -> Self {
        ProtoWalletAddress {
            index: value.index,
            cosigner_index: value.cosigner_index as u32,
            keychain: ProtoKeychain::from(value.keychain) as i32,
        }
    }
}

impl TryFrom<ProtoWalletAddress> for WalletAddress {
    type Error = WalletError;

    fn try_from(value: ProtoWalletAddress) -> WalletResult<Self> {
        let cosigner_index = u16::try_from(value.cosigner_index).map_err(|_| {
            WalletError::from(UserInputError::InvalidArgument {
                reason: format!(
                    "WalletAddress.cosigner_index {} exceeds u16::MAX",
                    value.cosigner_index
                ),
                location: ErrorLocation::capture(),
            })
        })?;
        Ok(WalletAddress {
            index: value.index,
            cosigner_index,
            keychain: ProtoKeychain::try_from(value.keychain)
                .unwrap_or(ProtoKeychain::External)
                .into(),
        })
    }
}

impl From<WalletOutpoint> for ProtoOutpoint {
    fn from(value: WalletOutpoint) -> ProtoOutpoint {
        ProtoOutpoint {
            transaction_id: value.transaction_id.to_string(),
            index: value.index,
        }
    }
}

impl TryFrom<ProtoOutpoint> for WalletOutpoint {
    type Error = WalletError;

    fn try_from(value: ProtoOutpoint) -> WalletResult<Self> {
        let transaction_id = Hash::from_str(&value.transaction_id).map_err(|e| {
            WalletError::from(UserInputError::InvalidArgument {
                reason: format!("Outpoint.transaction_id must be a 64-char hex hash: {e}"),
                location: ErrorLocation::capture(),
            })
        })?;
        Ok(WalletOutpoint {
            transaction_id,
            index: value.index,
        })
    }
}

pub fn transaction_outpoint_to_proto(value: TransactionOutpoint) -> ProtoTransactionOutpoint {
    ProtoTransactionOutpoint {
        transaction_id: value.transaction_id.as_bytes().to_vec().into(),
        index: value.index,
    }
}

pub fn transaction_outpoint_from_proto(
    value: ProtoTransactionOutpoint,
) -> WalletResult<TransactionOutpoint> {
    let id_bytes: [u8; 32] = value.transaction_id.as_ref().try_into().map_err(|_| {
        WalletError::from(UserInputError::InvalidArgument {
            reason: format!(
                "TransactionOutpoint.transaction_id must be 32 bytes, got {}",
                value.transaction_id.len()
            ),
            location: ErrorLocation::capture(),
        })
    })?;
    Ok(TransactionOutpoint {
        transaction_id: Hash::from_bytes(id_bytes),
        index: value.index,
    })
}

impl From<WalletUtxoEntry> for ProtoUtxoEntry {
    fn from(value: WalletUtxoEntry) -> ProtoUtxoEntry {
        ProtoUtxoEntry {
            amount: value.amount,
            script_public_key: Some(ProtoScriptPublicKey {
                version: value.script_public_key.version as u32,
                script_public_key: hex::encode(value.script_public_key.script()),
            }),
            block_daa_score: value.block_daa_score,
            is_coinbase: value.is_coinbase,
        }
    }
}

pub fn utxo_entry_to_proto(value: UtxoEntry) -> ProtoUtxoEntry {
    // Wallet-owned UTXOs are not covenant-bound; covenant_id is intentionally
    // dropped on the wire and reconstructed as None on receive.
    ProtoUtxoEntry {
        amount: value.amount,
        script_public_key: Some(script_public_key_to_proto(value.script_public_key)),
        block_daa_score: value.block_daa_score,
        is_coinbase: value.is_coinbase,
    }
}

pub fn utxo_entry_from_proto(value: ProtoUtxoEntry) -> WalletResult<UtxoEntry> {
    let script_public_key = value.script_public_key.unwrap_or_default();
    Ok(UtxoEntry {
        amount: value.amount,
        script_public_key: script_public_key_from_proto(script_public_key)?,
        block_daa_score: value.block_daa_score,
        is_coinbase: value.is_coinbase,
        covenant_id: None,
    })
}

impl WalletUtxo {
    pub fn into_proto(self, is_pending: bool, is_dust: bool) -> ProtoUtxo {
        ProtoUtxo {
            outpoint: Some(self.outpoint.into()),
            utxo_entry: Some(self.utxo_entry.into()),
            is_pending,
            is_dust,
        }
    }
}

pub fn script_public_key_to_proto(value: ScriptPublicKey) -> ProtoScriptPublicKey {
    ProtoScriptPublicKey {
        version: value.version as u32,
        script_public_key: hex::encode(value.script()),
    }
}

pub fn script_public_key_from_proto(value: ProtoScriptPublicKey) -> WalletResult<ScriptPublicKey> {
    let version = u16::try_from(value.version).map_err(|_| {
        WalletError::from(UserInputError::InvalidArgument {
            reason: format!("ScriptPublicKey.version {} exceeds u16::MAX", value.version),
            location: ErrorLocation::capture(),
        })
    })?;
    let script = hex::decode(&value.script_public_key).map_err(|e| {
        WalletError::from(UserInputError::InvalidArgument {
            reason: format!("ScriptPublicKey.script_public_key must be valid hex: {e}"),
            location: ErrorLocation::capture(),
        })
    })?;
    Ok(ScriptPublicKey::from_vec(version, script))
}

pub fn transaction_input_to_proto(value: TransactionInput) -> ProtoTransactionInput {
    // Always emit both mass fields; the parent Transaction's `version` tells
    // the receiver which one is authoritative. The unused field is zero.
    ProtoTransactionInput {
        previous_outpoint: Some(transaction_outpoint_to_proto(value.previous_outpoint)),
        signature_script: value.signature_script.into(),
        sequence: value.sequence,
        sig_op_count: u32::from(value.mass.sig_op_count().unwrap_or(0)),
        compute_budget: u32::from(value.mass.compute_budget().unwrap_or(0)),
    }
}

/// Reconstruct a `TransactionInput` from its proto form.
///
/// `tx_version` is the parent transaction's version; it selects which mass
/// field on the wire (`sig_op_count` vs `compute_budget`) is authoritative.
/// Returns an error when the wire-supplied mass field overflows the
/// consensus-side integer width — defends against truncating malformed or
/// malicious gRPC input on the signing path.
pub fn transaction_input_from_proto(
    value: ProtoTransactionInput,
    tx_version: u16,
) -> WalletResult<TransactionInput> {
    let previous_outpoint =
        transaction_outpoint_from_proto(value.previous_outpoint.unwrap_or_default())?;
    let signature_script = value.signature_script.to_vec();
    if TxInputMass::version_expects_compute_budget_field(tx_version) {
        // v1 inputs must carry compute_budget only. Reject a non-zero
        // sig_op_count on the wire — upstream consensus enforces this
        // invariant in `consensus/core/src/sign.rs` (see
        // `invalid_input_mass_variant`). Keeping the wallet side strict
        // matches the rest of the stack.
        if value.sig_op_count != 0 {
            return Err(WalletError::from(UserInputError::InvalidArgument {
                reason: format!(
                    "TransactionInput.sig_op_count must be 0 for v1 transactions, got {}",
                    value.sig_op_count
                ),
                location: ErrorLocation::capture(),
            }));
        }
        let compute_budget = u16::try_from(value.compute_budget).map_err(|_| {
            WalletError::from(UserInputError::InvalidArgument {
                reason: format!(
                    "TransactionInput.compute_budget {} exceeds u16 max",
                    value.compute_budget
                ),
                location: ErrorLocation::capture(),
            })
        })?;
        Ok(TransactionInput::new_with_compute_budget(
            previous_outpoint,
            signature_script,
            value.sequence,
            compute_budget,
        ))
    } else {
        // v0 inputs must carry sig_op_count only. Reject a non-zero
        // compute_budget so the wire shape matches consensus expectations.
        if value.compute_budget != 0 {
            return Err(WalletError::from(UserInputError::InvalidArgument {
                reason: format!(
                    "TransactionInput.compute_budget must be 0 for v0 transactions, got {}",
                    value.compute_budget
                ),
                location: ErrorLocation::capture(),
            }));
        }
        let sig_op_count = u8::try_from(value.sig_op_count).map_err(|_| {
            WalletError::from(UserInputError::InvalidArgument {
                reason: format!(
                    "TransactionInput.sig_op_count {} exceeds u8 max",
                    value.sig_op_count
                ),
                location: ErrorLocation::capture(),
            })
        })?;
        Ok(TransactionInput::new(
            previous_outpoint,
            signature_script,
            value.sequence,
            sig_op_count,
        ))
    }
}

pub fn transaction_output_to_proto(value: TransactionOutput) -> ProtoTransactionOutput {
    // Covenant bindings are not produced by this wallet; the field is
    // intentionally dropped on the wire and reconstructed as None on receive.
    ProtoTransactionOutput {
        value: value.value,
        script_public_key: Some(script_public_key_to_proto(value.script_public_key)),
    }
}

pub fn transaction_output_from_proto(
    value: ProtoTransactionOutput,
) -> WalletResult<TransactionOutput> {
    Ok(TransactionOutput {
        value: value.value,
        script_public_key: script_public_key_from_proto(
            value.script_public_key.unwrap_or_default(),
        )?,
        covenant: None,
    })
}

pub fn transaction_to_proto(value: Transaction) -> ProtoTransaction {
    let id = value.id();
    let mass = value.mass();
    let subnetwork_id: &[u8] = value.subnetwork_id.as_ref();

    ProtoTransaction {
        version: value.version as u32,
        inputs: value
            .inputs
            .into_iter()
            .map(transaction_input_to_proto)
            .collect(),
        outputs: value
            .outputs
            .into_iter()
            .map(transaction_output_to_proto)
            .collect(),
        lock_time: value.lock_time,
        subnetwork_id: subnetwork_id.to_vec().into(),
        gas: value.gas,
        payload: value.payload.into(),
        mass,
        id: id.as_bytes().to_vec().into(),
    }
}

pub fn transaction_from_proto(value: ProtoTransaction) -> WalletResult<Transaction> {
    let tx_version = u16::try_from(value.version).map_err(|_| {
        WalletError::from(UserInputError::InvalidArgument {
            reason: format!("Transaction.version {} exceeds u16::MAX", value.version),
            location: ErrorLocation::capture(),
        })
    })?;
    let subnetwork_bytes: [u8; SUBNETWORK_ID_SIZE] =
        value.subnetwork_id.as_ref().try_into().map_err(|_| {
            WalletError::from(UserInputError::InvalidArgument {
                reason: format!(
                    "Transaction.subnetwork_id must be {} bytes, got {}",
                    SUBNETWORK_ID_SIZE,
                    value.subnetwork_id.len()
                ),
                location: ErrorLocation::capture(),
            })
        })?;
    let inputs = value
        .inputs
        .into_iter()
        .map(|input| transaction_input_from_proto(input, tx_version))
        .collect::<WalletResult<Vec<_>>>()?;
    let outputs = value
        .outputs
        .into_iter()
        .map(transaction_output_from_proto)
        .collect::<WalletResult<Vec<_>>>()?;
    let mut transaction = Transaction::new_non_finalized(
        tx_version,
        inputs,
        outputs,
        value.lock_time,
        SubnetworkId::from_bytes(subnetwork_bytes),
        value.gas,
        value.payload.to_vec(),
    );
    transaction.set_mass(value.mass);
    transaction.finalize();
    Ok(transaction)
}

pub fn optional_utxo_entry_to_proto(value: Option<UtxoEntry>) -> ProtoOptionalUtxoEntry {
    ProtoOptionalUtxoEntry {
        entry: value.map(utxo_entry_to_proto),
    }
}

pub fn optional_utxo_entry_from_proto(
    value: ProtoOptionalUtxoEntry,
) -> WalletResult<Option<UtxoEntry>> {
    value.entry.map(utxo_entry_from_proto).transpose()
}

pub fn signable_transaction_to_proto(value: SignableTransaction) -> ProtoSignableTransaction {
    ProtoSignableTransaction {
        tx: Some(transaction_to_proto(value.tx)),
        entries: value
            .entries
            .into_iter()
            .map(optional_utxo_entry_to_proto)
            .collect(),
        calculated_fee: value.calculated_fee,
        calculated_non_contextual_masses: value.calculated_non_contextual_masses.map(|m| {
            ProtoNonContextualMasses {
                compute_mass: m.compute_mass,
                transient_mass: m.transient_mass,
            }
        }),
    }
}

pub fn signable_transaction_from_proto(
    value: ProtoSignableTransaction,
) -> WalletResult<SignableTransaction> {
    let tx = transaction_from_proto(value.tx.unwrap_or_default())?;
    let entries: Vec<Option<UtxoEntry>> = value
        .entries
        .into_iter()
        .map(optional_utxo_entry_from_proto)
        .collect::<WalletResult<Vec<_>>>()?;
    // Upstream `SignableTransaction::as_verifiable` asserts (panics) when
    // `entries.len() != inputs.len()` or any entry is None. Validate here
    // so a malformed wire payload turns into a typed gRPC InvalidArgument
    // instead of a daemon panic on the broadcast/sign path.
    if entries.len() != tx.inputs.len() {
        return Err(WalletError::from(UserInputError::InvalidArgument {
            reason: format!(
                "SignableTransaction.entries length {} does not match inputs length {}",
                entries.len(),
                tx.inputs.len()
            ),
            location: ErrorLocation::capture(),
        }));
    }
    if entries.iter().any(|e| e.is_none()) {
        return Err(WalletError::from(UserInputError::InvalidArgument {
            reason: "SignableTransaction.entries must not contain None for verification"
                .to_string(),
            location: ErrorLocation::capture(),
        }));
    }
    Ok(SignableTransaction {
        tx,
        entries,
        calculated_fee: value.calculated_fee,
        calculated_non_contextual_masses: value.calculated_non_contextual_masses.map(|m| {
            kaspa_consensus_core::mass::NonContextualMasses {
                compute_mass: m.compute_mass,
                transient_mass: m.transient_mass,
            }
        }),
    })
}

pub fn signed_transaction_to_proto(value: Signed) -> ProtoSignedTransaction {
    match value {
        Signed::Fully(tx) => ProtoSignedTransaction {
            signed: Some(signed_transaction::Signed::Fully(
                signable_transaction_to_proto(tx),
            )),
        },
        Signed::Partially(tx) => ProtoSignedTransaction {
            signed: Some(signed_transaction::Signed::Partially(
                signable_transaction_to_proto(tx),
            )),
        },
    }
}

pub fn signed_transaction_from_proto(value: ProtoSignedTransaction) -> WalletResult<Signed> {
    match value.signed {
        Some(signed_transaction::Signed::Fully(tx)) => {
            Ok(Signed::Fully(signable_transaction_from_proto(tx)?))
        }
        Some(signed_transaction::Signed::Partially(tx)) => {
            Ok(Signed::Partially(signable_transaction_from_proto(tx)?))
        }
        None => Err(WalletError::from(UserInputError::InvalidArgument {
            reason: "SignedTransaction.signed oneof must be set".to_string(),
            location: ErrorLocation::capture(),
        })),
    }
}

impl From<WalletSignableTransaction> for ProtoWalletSignableTransaction {
    fn from(value: WalletSignableTransaction) -> Self {
        ProtoWalletSignableTransaction {
            transaction: Some(signed_transaction_to_proto(value.transaction.into())),
            derivation_paths: value
                .derivation_paths
                .into_iter()
                .map(derivation_path_to_proto)
                .collect(),
            address_by_input_index: value
                .address_by_input_index
                .into_iter()
                .map(Into::into)
                .collect(),
            address_by_output_index: value
                .address_by_output_index
                .into_iter()
                .map(|addr| addr.to_string())
                .collect(),
        }
    }
}

impl TryFrom<ProtoWalletSignableTransaction> for WalletSignableTransaction {
    type Error = WalletError;

    fn try_from(value: ProtoWalletSignableTransaction) -> WalletResult<Self> {
        let transaction =
            signed_transaction_from_proto(value.transaction.unwrap_or_default())?.into();
        let address_by_output_index = value
            .address_by_output_index
            .into_iter()
            .map(|s| {
                Address::try_from(s.as_str()).map_err(|e| {
                    WalletError::from(UserInputError::InvalidAddress {
                        input: s,
                        reason: e.to_string(),
                        location: ErrorLocation::capture(),
                    })
                })
            })
            .collect::<WalletResult<Vec<_>>>()?;
        // Dedup derivation paths on the wire boundary. The signing path
        // iterates this collection and derives one private key per (path,
        // mnemonic) pair; an unauthenticated caller posting a transaction
        // with thousands of duplicated paths would otherwise inflate the
        // wallet's signing cost into a CPU-DoS vector. The wallet's own
        // construction site already dedups via the same predicate.
        let mut seen_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut derivation_paths: Vec<DerivationPath> = Vec::new();
        for proto_path in value.derivation_paths {
            let path = derivation_path_from_proto(proto_path);
            if seen_paths.insert(path.to_string()) {
                derivation_paths.push(path);
            }
        }
        let address_by_input_index = value
            .address_by_input_index
            .into_iter()
            .map(WalletAddress::try_from)
            .collect::<WalletResult<Vec<_>>>()?;
        Ok(WalletSignableTransaction {
            transaction,
            derivation_paths,
            address_by_input_index,
            address_by_output_index,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `[u8; 32]` hash bytes deterministic enough for fixture use.
    fn fixture_outpoint() -> TransactionOutpoint {
        TransactionOutpoint::new(Hash::from_bytes([7u8; 32]), 0)
    }

    #[test]
    fn v0_input_roundtrips_sig_op_count() {
        let original = TransactionInput::new(
            fixture_outpoint(),
            vec![0x42, 0x43],
            99,
            /* sig_op_count */ 5,
        );
        let original_outpoint = original.previous_outpoint;
        let original_script = original.signature_script.clone();
        let original_sequence = original.sequence;
        let proto = transaction_input_to_proto(original);
        assert_eq!(proto.sig_op_count, 5);
        assert_eq!(proto.compute_budget, 0);

        let restored =
            transaction_input_from_proto(proto, /* tx_version */ 0).expect("valid v0 input");
        assert_eq!(restored.mass.sig_op_count(), Some(5));
        assert_eq!(restored.mass.compute_budget(), None);
        assert_eq!(restored.previous_outpoint, original_outpoint);
        assert_eq!(restored.signature_script, original_script);
        assert_eq!(restored.sequence, original_sequence);
    }

    #[test]
    fn v1_input_roundtrips_compute_budget() {
        let original = TransactionInput::new_with_compute_budget(
            fixture_outpoint(),
            vec![],
            123,
            /* compute_budget */ 42,
        );
        let proto = transaction_input_to_proto(original);
        assert_eq!(proto.sig_op_count, 0);
        assert_eq!(proto.compute_budget, 42);

        let restored =
            transaction_input_from_proto(proto, /* tx_version */ 1).expect("valid v1 input");
        assert_eq!(restored.mass.compute_budget(), Some(42));
        assert_eq!(restored.mass.sig_op_count(), None);
    }

    #[test]
    fn from_proto_picks_mass_field_by_tx_version() {
        // Proto carries both fields but the off-version field must be 0;
        // the consumer's `tx_version` selects which on-the-wire field is
        // authoritative and the conversion enforces the other is unused.
        let v0_proto = ProtoTransactionInput {
            previous_outpoint: Some(transaction_outpoint_to_proto(fixture_outpoint())),
            signature_script: vec![].into(),
            sequence: 0,
            sig_op_count: 7,
            compute_budget: 0,
        };
        let v1_proto = ProtoTransactionInput {
            previous_outpoint: Some(transaction_outpoint_to_proto(fixture_outpoint())),
            signature_script: vec![].into(),
            sequence: 0,
            sig_op_count: 0,
            compute_budget: 11,
        };

        let as_v0 = transaction_input_from_proto(v0_proto, 0).expect("valid v0");
        assert_eq!(as_v0.mass.sig_op_count(), Some(7));

        let as_v1 = transaction_input_from_proto(v1_proto, 1).expect("valid v1");
        assert_eq!(as_v1.mass.compute_budget(), Some(11));
    }

    #[test]
    fn from_proto_v1_rejects_non_zero_sig_op_count() {
        let proto = ProtoTransactionInput {
            previous_outpoint: Some(transaction_outpoint_to_proto(fixture_outpoint())),
            signature_script: vec![].into(),
            sequence: 0,
            sig_op_count: 1,
            compute_budget: 10,
        };
        let err = transaction_input_from_proto(proto, 1)
            .expect_err("v1 with non-zero sig_op_count must be rejected");
        assert!(
            matches!(
                err,
                WalletError::UserInput(UserInputError::InvalidArgument { .. })
            ),
            "expected InvalidArgument, got: {err}"
        );
    }

    #[test]
    fn from_proto_v0_rejects_non_zero_compute_budget() {
        let proto = ProtoTransactionInput {
            previous_outpoint: Some(transaction_outpoint_to_proto(fixture_outpoint())),
            signature_script: vec![].into(),
            sequence: 0,
            sig_op_count: 1,
            compute_budget: 1,
        };
        let err = transaction_input_from_proto(proto, 0)
            .expect_err("v0 with non-zero compute_budget must be rejected");
        assert!(
            matches!(
                err,
                WalletError::UserInput(UserInputError::InvalidArgument { .. })
            ),
            "expected InvalidArgument, got: {err}"
        );
    }

    #[test]
    fn from_proto_rejects_compute_budget_overflowing_u16() {
        let proto = ProtoTransactionInput {
            previous_outpoint: Some(transaction_outpoint_to_proto(fixture_outpoint())),
            signature_script: vec![].into(),
            sequence: 0,
            sig_op_count: 0,
            compute_budget: u32::from(u16::MAX) + 1,
        };
        let err = transaction_input_from_proto(proto, 1)
            .expect_err("compute_budget > u16::MAX must be rejected");
        assert!(
            matches!(
                err,
                WalletError::UserInput(UserInputError::InvalidArgument { .. })
            ),
            "expected InvalidArgument, got: {err}"
        );
    }

    #[test]
    fn from_proto_rejects_sig_op_count_overflowing_u8() {
        let proto = ProtoTransactionInput {
            previous_outpoint: Some(transaction_outpoint_to_proto(fixture_outpoint())),
            signature_script: vec![].into(),
            sequence: 0,
            sig_op_count: u32::from(u8::MAX) + 1,
            compute_budget: 0,
        };
        let err = transaction_input_from_proto(proto, 0)
            .expect_err("sig_op_count > u8::MAX must be rejected");
        assert!(
            matches!(
                err,
                WalletError::UserInput(UserInputError::InvalidArgument { .. })
            ),
            "expected InvalidArgument, got: {err}"
        );
    }

    #[test]
    fn signed_transaction_from_proto_rejects_missing_oneof() {
        let proto = ProtoSignedTransaction { signed: None };
        // `Signed` does not derive Debug upstream, so unwrap_err's bound
        // can't be satisfied — match on the result manually.
        match signed_transaction_from_proto(proto) {
            Ok(_) => panic!("missing oneof must be rejected"),
            Err(WalletError::UserInput(UserInputError::InvalidArgument { .. })) => {}
            Err(other) => panic!("expected InvalidArgument, got: {other}"),
        }
    }
}
