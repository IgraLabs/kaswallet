use crate::model::{Keychain, WalletAddress, WalletOutpoint, WalletSignableTransaction, WalletUtxo, WalletUtxoEntry};
use kaspa_addresses::Address;
use kaspa_bip32::{ChildNumber, DerivationPath};
use kaspa_consensus_core::sign::Signed;
use kaspa_consensus_core::subnets::SubnetworkId;
use kaspa_consensus_core::tx::{
    ScriptPublicKey, SignableTransaction, Transaction, TransactionInput, TransactionOutpoint,
    TransactionOutput, UtxoEntry,
};
use kaspa_hashes::Hash;
use proto::kaswallet_proto::{
    signed_transaction, DerivationPath as ProtoDerivationPath, Keychain as ProtoKeychain,
    NonContextualMasses as ProtoNonContextualMasses, OptionalUtxoEntry as ProtoOptionalUtxoEntry,
    Outpoint as ProtoOutpoint, ScriptPublicKey as ProtoScriptPublicKey,
    SignableTransaction as ProtoSignableTransaction, SignedTransaction as ProtoSignedTransaction,
    Transaction as ProtoTransaction, TransactionInput as ProtoTransactionInput,
    TransactionOutpoint as ProtoTransactionOutpoint, TransactionOutput as ProtoTransactionOutput,
    Utxo as ProtoUtxo, UtxoEntry as ProtoUtxoEntry, WalletAddress as ProtoWalletAddress,
    WalletSignableTransaction as ProtoWalletSignableTransaction,
};
use std::str::FromStr;

pub fn derivation_path_to_proto(value: DerivationPath) -> ProtoDerivationPath {
    ProtoDerivationPath {
        path: value.as_ref().iter().map(|child_number| child_number.0).collect(),
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

impl From<ProtoWalletAddress> for WalletAddress {
    fn from(value: ProtoWalletAddress) -> Self {
        WalletAddress {
            index: value.index,
            cosigner_index: value.cosigner_index as u16,
            keychain: ProtoKeychain::try_from(value.keychain)
                .unwrap_or(ProtoKeychain::External)
                .into(),
        }
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

impl From<ProtoOutpoint> for WalletOutpoint {
    fn from(value: ProtoOutpoint) -> WalletOutpoint {
        WalletOutpoint {
            transaction_id: Hash::from_str(&value.transaction_id).unwrap(),
            index: value.index,
        }
    }
}

pub fn transaction_outpoint_to_proto(value: TransactionOutpoint) -> ProtoTransactionOutpoint {
    ProtoTransactionOutpoint {
        transaction_id: value.transaction_id.as_bytes().to_vec().into(),
        index: value.index,
    }
}

pub fn transaction_outpoint_from_proto(value: ProtoTransactionOutpoint) -> TransactionOutpoint {
    TransactionOutpoint {
        transaction_id: Hash::from_bytes(value.transaction_id.to_vec().as_slice().try_into().unwrap()),
        index: value.index,
    }
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
    ProtoUtxoEntry {
        amount: value.amount,
        script_public_key: Some(script_public_key_to_proto(value.script_public_key)),
        block_daa_score: value.block_daa_score,
        is_coinbase: value.is_coinbase,
    }
}

pub fn utxo_entry_from_proto(value: ProtoUtxoEntry) -> UtxoEntry {
    let script_public_key = value.script_public_key.unwrap_or_default();
    UtxoEntry {
        amount: value.amount,
        script_public_key: script_public_key_from_proto(script_public_key),
        block_daa_score: value.block_daa_score,
        is_coinbase: value.is_coinbase,
    }
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

pub fn script_public_key_from_proto(value: ProtoScriptPublicKey) -> ScriptPublicKey {
    ScriptPublicKey::from_vec(
        value.version as u16,
        hex::decode(value.script_public_key).unwrap_or_default(),
    )
}

pub fn transaction_input_to_proto(value: TransactionInput) -> ProtoTransactionInput {
    ProtoTransactionInput {
        previous_outpoint: Some(transaction_outpoint_to_proto(value.previous_outpoint)),
        signature_script: value.signature_script.into(),
        sequence: value.sequence,
        sig_op_count: value.sig_op_count as u32,
    }
}

pub fn transaction_input_from_proto(value: ProtoTransactionInput) -> TransactionInput {
    TransactionInput {
        previous_outpoint: transaction_outpoint_from_proto(value.previous_outpoint.unwrap_or_default()),
        signature_script: value.signature_script.to_vec(),
        sequence: value.sequence,
        sig_op_count: value.sig_op_count as u8,
    }
}

pub fn transaction_output_to_proto(value: TransactionOutput) -> ProtoTransactionOutput {
    ProtoTransactionOutput {
        value: value.value,
        script_public_key: Some(script_public_key_to_proto(value.script_public_key)),
    }
}

pub fn transaction_output_from_proto(value: ProtoTransactionOutput) -> TransactionOutput {
    TransactionOutput {
        value: value.value,
        script_public_key: script_public_key_from_proto(value.script_public_key.unwrap_or_default()),
    }
}

pub fn transaction_to_proto(value: Transaction) -> ProtoTransaction {
    let id = value.id();
    let mass = value.mass();
    let subnetwork_id: &[u8] = value.subnetwork_id.as_ref();

    ProtoTransaction {
        version: value.version as u32,
        inputs: value.inputs.into_iter().map(transaction_input_to_proto).collect(),
        outputs: value.outputs.into_iter().map(transaction_output_to_proto).collect(),
        lock_time: value.lock_time,
        subnetwork_id: subnetwork_id.to_vec().into(),
        gas: value.gas,
        payload: value.payload.into(),
        mass,
        id: id.as_bytes().to_vec().into(),
    }
}

pub fn transaction_from_proto(value: ProtoTransaction) -> Transaction {
    let mut transaction = Transaction::new_non_finalized(
        value.version as u16,
        value.inputs.into_iter().map(transaction_input_from_proto).collect(),
        value.outputs.into_iter().map(transaction_output_from_proto).collect(),
        value.lock_time,
        SubnetworkId::from_bytes(value.subnetwork_id.to_vec().as_slice().try_into().unwrap()),
        value.gas,
        value.payload.to_vec(),
    );
    transaction.set_mass(value.mass);
    transaction.finalize();
    transaction
}

pub fn optional_utxo_entry_to_proto(value: Option<UtxoEntry>) -> ProtoOptionalUtxoEntry {
    ProtoOptionalUtxoEntry {
        entry: value.map(utxo_entry_to_proto),
    }
}

pub fn optional_utxo_entry_from_proto(value: ProtoOptionalUtxoEntry) -> Option<UtxoEntry> {
    value.entry.map(utxo_entry_from_proto)
}

pub fn signable_transaction_to_proto(value: SignableTransaction) -> ProtoSignableTransaction {
    ProtoSignableTransaction {
        tx: Some(transaction_to_proto(value.tx)),
        entries: value.entries.into_iter().map(optional_utxo_entry_to_proto).collect(),
        calculated_fee: value.calculated_fee,
        calculated_non_contextual_masses: value.calculated_non_contextual_masses.map(|m| {
            ProtoNonContextualMasses {
                compute_mass: m.compute_mass,
                transient_mass: m.transient_mass,
            }
        }),
    }
}

pub fn signable_transaction_from_proto(value: ProtoSignableTransaction) -> SignableTransaction {
    SignableTransaction {
        tx: transaction_from_proto(value.tx.unwrap_or_default()),
        entries: value.entries.into_iter().map(optional_utxo_entry_from_proto).collect(),
        calculated_fee: value.calculated_fee,
        calculated_non_contextual_masses: value.calculated_non_contextual_masses.map(|m| {
            kaspa_consensus_core::mass::NonContextualMasses {
                compute_mass: m.compute_mass,
                transient_mass: m.transient_mass,
            }
        }),
    }
}

pub fn signed_transaction_to_proto(value: Signed) -> ProtoSignedTransaction {
    match value {
        Signed::Fully(tx) => ProtoSignedTransaction {
            signed: Some(signed_transaction::Signed::Fully(signable_transaction_to_proto(tx))),
        },
        Signed::Partially(tx) => ProtoSignedTransaction {
            signed: Some(signed_transaction::Signed::Partially(signable_transaction_to_proto(tx))),
        },
    }
}

pub fn signed_transaction_from_proto(value: ProtoSignedTransaction) -> Signed {
    match value.signed {
        Some(signed_transaction::Signed::Fully(tx)) => Signed::Fully(signable_transaction_from_proto(tx)),
        Some(signed_transaction::Signed::Partially(tx)) => Signed::Partially(signable_transaction_from_proto(tx)),
        None => panic!("SignedTransaction must have either fully or partially set"),
    }
}

impl From<WalletSignableTransaction> for ProtoWalletSignableTransaction {
    fn from(value: WalletSignableTransaction) -> Self {
        ProtoWalletSignableTransaction {
            transaction: Some(signed_transaction_to_proto(value.transaction)),
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

impl From<ProtoWalletSignableTransaction> for WalletSignableTransaction {
    fn from(value: ProtoWalletSignableTransaction) -> Self {
        WalletSignableTransaction {
            transaction: signed_transaction_from_proto(value.transaction.unwrap_or_default()),
            derivation_paths: value
                .derivation_paths
                .into_iter()
                .map(derivation_path_from_proto)
                .collect(),
            address_by_input_index: value
                .address_by_input_index
                .into_iter()
                .map(Into::into)
                .collect(),
            address_by_output_index: value
                .address_by_output_index
                .into_iter()
                .map(|s| Address::try_from(s.as_str()).unwrap())
                .collect(),
        }
    }
}
