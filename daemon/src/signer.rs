//! Wallet signing as free functions, usable both by the daemon's gRPC
//! service and by standalone binaries (e.g. `kaswallet-batch-send`) that
//! sign without a running daemon. The service layer delegates here so
//! there is exactly one signing implementation.

use common::error_location::ErrorLocation;
use common::errors::{CryptoError, TransactionError, WalletError, WalletResult};
use common::keys::{Keys, master_key_path};
use common::model::WalletSignableTransaction;
use itertools::Itertools;
use kaspa_bip32::{ExtendedPrivateKey, Mnemonic, SecretKey, secp256k1};
use kaspa_consensus_core::hashing::sighash::{
    SigHashReusedValuesUnsync, calc_schnorr_signature_hash,
};
use kaspa_consensus_core::hashing::sighash_type::SIG_HASH_ALL;
use kaspa_consensus_core::sign::Signed;
use kaspa_consensus_core::sign::Signed::{Fully, Partially};
use kaspa_consensus_core::tx::SignableTransaction;
use secrecy::SecretString;
use std::collections::BTreeMap;
use std::iter::once;
use tracing::debug;

/// Decrypt the keys file's mnemonics with `password` and derive one master
/// extended private key per mnemonic. Splitting this from the signing step
/// lets callers validate the password once, up front (before any network
/// work), and reuse the derived keys across multiple sign calls without
/// re-running the argon2 KDF.
pub fn decrypt_private_keys(
    keys: &Keys,
    password: &SecretString,
) -> WalletResult<Vec<ExtendedPrivateKey<SecretKey>>> {
    let mnemonics = keys.decrypt_mnemonics(password)?;
    let is_multisig = mnemonics.len() > 1;
    mnemonics
        .iter()
        .map(|mnemonic| mnemonic_to_private_key(mnemonic, is_multisig))
        .collect()
}

/// Sign every transaction with pre-derived master private keys
/// (see [`decrypt_private_keys`]).
pub fn sign_transactions_with_keys(
    private_keys: &[ExtendedPrivateKey<SecretKey>],
    unsigned_transactions: Vec<WalletSignableTransaction>,
) -> WalletResult<Vec<WalletSignableTransaction>> {
    let mut signed_transactions = vec![];
    for unsigned_transaction in unsigned_transactions {
        let derivation_paths = unsigned_transaction.derivation_paths.clone();
        let address_by_input_index = unsigned_transaction.address_by_input_index.clone();
        let address_by_output_index = unsigned_transaction.address_by_output_index.clone();

        let signed_transaction = sign_transaction(unsigned_transaction, private_keys)?;
        let wallet_signed_transaction = WalletSignableTransaction::new(
            signed_transaction.into(),
            derivation_paths,
            address_by_input_index,
            address_by_output_index,
        );

        signed_transactions.push(wallet_signed_transaction);
    }

    Ok(signed_transactions)
}

/// Decrypt keys with `password` and sign every transaction. Convenience
/// wrapper over [`decrypt_private_keys`] + [`sign_transactions_with_keys`].
pub fn sign_transactions(
    keys: &Keys,
    unsigned_transactions: Vec<WalletSignableTransaction>,
    password: &SecretString,
) -> WalletResult<Vec<WalletSignableTransaction>> {
    let private_keys = decrypt_private_keys(keys, password)?;
    sign_transactions_with_keys(&private_keys, unsigned_transactions)
}

/// Sign a single transaction: derive the per-address child key for every
/// recorded derivation path, sign matching inputs, and sanity-verify fully
/// signed results.
pub fn sign_transaction(
    unsigned_transaction: WalletSignableTransaction,
    extended_private_keys: &[ExtendedPrivateKey<SecretKey>],
) -> WalletResult<Signed> {
    let mut private_keys = vec![];
    for derivation_path in &unsigned_transaction.derivation_paths {
        for extended_private_key in extended_private_keys.iter() {
            let private_key = extended_private_key
                .clone()
                .derive_path(derivation_path)
                .map_err(|e| CryptoError::Bip32Derivation {
                    reason: e.to_string(),
                    location: ErrorLocation::capture(),
                })?;
            private_keys.push(private_key.private_key().secret_bytes());
        }
    }

    let signable_transaction = unsigned_transaction.transaction;
    let signed_transaction = sign_with_multiple(signable_transaction.into_inner(), &private_keys);

    sanity_check_verify(&signed_transaction)?;
    Ok(signed_transaction)
}

fn sanity_check_verify(signed_transaction: &Signed) -> WalletResult<()> {
    let signable = match signed_transaction {
        Signed::Fully(tx) => {
            debug!("Transaction is fully signed");
            tx
        }
        Signed::Partially(_) => {
            debug!("Transaction is partially signed, so can't verify");
            return Ok(());
        }
    };
    let verifiable_transaction = &signable.as_verifiable();
    // Whole-transaction verify failure has no per-input attribution; use
    // the dedicated `VerifyFailed` variant rather than fabricating
    // `input_index: 0` (which the reviewer flagged as misleading).
    kaspa_consensus_core::sign::verify(verifiable_transaction).map_err(|e| {
        WalletError::from(TransactionError::VerifyFailed {
            reason: e.to_string(),
            location: ErrorLocation::capture(),
        })
    })?;

    Ok(())
}

// Public helper function to convert a single mnemonic to master private key
pub fn mnemonic_to_private_key(
    mnemonic: &Mnemonic,
    is_multisig: bool,
) -> WalletResult<ExtendedPrivateKey<SecretKey>> {
    let seed = mnemonic.to_seed("");
    let x_private_key =
        ExtendedPrivateKey::new(seed).map_err(|e| CryptoError::Bip32Derivation {
            reason: e.to_string(),
            location: ErrorLocation::capture(),
        })?;
    let master_key_derivation_path = master_key_path(is_multisig);
    let private_key = x_private_key
        .derive_path(&master_key_derivation_path)
        .map_err(|e| CryptoError::Bip32Derivation {
            reason: e.to_string(),
            location: ErrorLocation::capture(),
        })?;
    Ok(private_key)
}

// This is a copy of the sign_with_multiple_v2 function from the wallet core
// With the following addition: Update the sig_op_count
pub fn sign_with_multiple(mut mutable_tx: SignableTransaction, privkeys: &[[u8; 32]]) -> Signed {
    let mut map = BTreeMap::new();
    for privkey in privkeys {
        let schnorr_key =
            secp256k1::Keypair::from_seckey_slice(secp256k1::SECP256K1, privkey).unwrap();
        let schnorr_public_key = schnorr_key.public_key().x_only_public_key().0;
        let script_pub_key_script = once(0x20)
            .chain(schnorr_public_key.serialize())
            .chain(once(0xac))
            .collect_vec();
        map.insert(script_pub_key_script, schnorr_key);
    }

    let reused_values = SigHashReusedValuesUnsync::new();
    let mut additional_signatures_required = false;
    for i in 0..mutable_tx.tx.inputs.len() {
        let script = mutable_tx.entries[i]
            .as_ref()
            .unwrap()
            .script_public_key
            .script();
        if let Some(schnorr_key) = map.get(script) {
            let sig_hash = calc_schnorr_signature_hash(
                &mutable_tx.as_verifiable(),
                i,
                SIG_HASH_ALL,
                &reused_values,
            );
            let msg =
                secp256k1::Message::from_digest_slice(sig_hash.as_bytes().as_slice()).unwrap();
            let sig: [u8; 64] = *schnorr_key.sign_schnorr(msg).as_ref();
            // This represents OP_DATA_65 <SIGNATURE+SIGHASH_TYPE> (since signature length is 64 bytes and SIGHASH_TYPE is one byte)
            mutable_tx.tx.inputs[i].signature_script = once(65u8)
                .chain(sig)
                .chain([SIG_HASH_ALL.to_u8()])
                .collect();
        } else {
            additional_signatures_required = true;
        }
    }
    if additional_signatures_required {
        Partially(mutable_tx)
    } else {
        Fully(mutable_tx)
    }
}
