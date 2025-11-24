use crate::service::kaswallet_service::KasWalletService;
use common::errors::WalletError::SanityCheckFailed;
use common::errors::{ResultExt, WalletResult};
use common::keys::master_key_path;
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
use log::debug;
use proto::kaswallet_proto::{SignRequest, SignResponse};
use std::collections::BTreeMap;
use std::iter::once;

impl KasWalletService {
    pub(crate) async fn sign(&self, request: SignRequest) -> WalletResult<SignResponse> {
        let unsigned_transactions: Vec<_> = request
            .unsigned_transactions
            .into_iter()
            .map(Into::into)
            .collect();

        let signed_transactions = self
            .sign_transactions(unsigned_transactions, &request.password)
            .await?;

        Ok(SignResponse {
            signed_transactions: signed_transactions.into_iter().map(Into::into).collect(),
        })
    }

    pub(crate) async fn sign_transactions(
        &self,
        unsigned_transactions: Vec<WalletSignableTransaction>,
        password: &String,
    ) -> WalletResult<Vec<WalletSignableTransaction>> {
        let mnemonics = self
            .keys
            .decrypt_mnemonics(password)
            .to_wallet_result_user_input()?;
        let extended_private_keys = Self::mnemonics_to_private_keys(&mnemonics)?;

        let mut signed_transactions = vec![];
        for unsigned_transaction in unsigned_transactions {
            let derivation_paths = unsigned_transaction.derivation_paths.clone();
            let address_by_input_index = unsigned_transaction.address_by_input_index.clone();
            let address_by_output_index = unsigned_transaction.address_by_output_index.clone();

            let signed_transaction = self
                .sign_transaction(unsigned_transaction, &extended_private_keys)
                .to_wallet_result_user_input()?;
            let wallet_signed_transaction = WalletSignableTransaction::new(
                signed_transaction,
                derivation_paths,
                address_by_input_index,
                address_by_output_index,
            );

            signed_transactions.push(wallet_signed_transaction);
        }

        Ok(signed_transactions)
    }

    pub(crate) fn sign_transaction(
        &self,
        unsigned_transaction: WalletSignableTransaction,
        extended_private_keys: &[ExtendedPrivateKey<SecretKey>],
    ) -> WalletResult<Signed> {
        let mut private_keys = vec![];
        for derivation_path in &unsigned_transaction.derivation_paths {
            for extended_private_key in extended_private_keys.iter() {
                let private_key = extended_private_key
                    .clone()
                    .derive_path(derivation_path)
                    .to_wallet_result_internal()?;
                private_keys.push(private_key.private_key().secret_bytes());
            }
        }

        let signable_transaction = unsigned_transaction.transaction;
        let signed_transaction = sign_with_multiple(signable_transaction.unwrap(), &private_keys);

        Self::sanity_check_verify(&signed_transaction)?;
        Ok(signed_transaction)
    }

    fn sanity_check_verify(signed_transaction: &Signed) -> WalletResult<()> {
        if let Fully(_) = signed_transaction {
            debug!("Transaction is fully signed");
        }
        if let Partially(_) = signed_transaction {
            debug!("Transaction is partially signed, so can't verify");
            return Ok(());
        }
        let verifiable_transaction = &signed_transaction.unwrap_ref().as_verifiable();
        kaspa_consensus_core::sign::verify(verifiable_transaction).map_err(|e| {
            SanityCheckFailed(format!(
                "Signed transaction does not verify correctly: {}",
                e
            ))
        })?;

        Ok(())
    }

    fn mnemonics_to_private_keys(
        mnemonics: &Vec<Mnemonic>,
    ) -> WalletResult<Vec<ExtendedPrivateKey<SecretKey>>> {
        let is_multisig = mnemonics.len() > 1;
        mnemonics
            .iter()
            .map(|mnemonic| mnemonic_to_private_key(mnemonic, is_multisig))
            .collect()
    }
}

// Public helper function to convert a single mnemonic to master private key
pub fn mnemonic_to_private_key(
    mnemonic: &Mnemonic,
    is_multisig: bool,
) -> WalletResult<ExtendedPrivateKey<SecretKey>> {
    let seed = mnemonic.to_seed("");
    let x_private_key = ExtendedPrivateKey::new(seed).to_wallet_result_internal()?;
    let master_key_derivation_path = master_key_path(is_multisig);
    let private_key = x_private_key
        .derive_path(&master_key_derivation_path)
        .to_wallet_result_internal()?;
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
            .chain(schnorr_public_key.serialize().into_iter())
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
