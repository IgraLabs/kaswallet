use crate::args::Args;
use common::encrypted_mnemonic::EncryptedMnemonic;
use common::errors::WalletError::InternalServerError;
use common::errors::WalletResult;
use common::keys::{master_key_path, Keys, KEY_FILE_VERSION};
use kaspa_bip32::secp256k1::PublicKey;
use kaspa_bip32::{ExtendedPrivateKey, ExtendedPublicKey, Mnemonic, Prefix, SecretKey};
use std::sync::Arc;

pub fn generate_keys_file(
    args: Arc<Args>,
    keys_file_path: String,
    mnemonics: Arc<Vec<Mnemonic>>,
    password: String,
    extra_public_keys: Vec<ExtendedPublicKey<PublicKey>>,
) -> WalletResult<Keys> {
    let prefix = Prefix::from(args.network_id());
    let is_multisig = mnemonics.len() > 1;
    let encrypted_mnemonics = encrypt_mnemonics(&password, &mnemonics)?;

    let x_public_keys = extract_x_public_keys(mnemonics, is_multisig);

    for (i, x_public_key) in x_public_keys.iter().enumerate() {
        println!(
            "Extended public key of mnemonic#{}: {}",
            i + 1,
            x_public_key.to_string(Some(prefix))
        );
    }

    let mut all_public_keys = x_public_keys.clone();
    all_public_keys.extend(extra_public_keys);

    let cosigner_index: u16 = if x_public_keys.is_empty() {
        0
    } else {
        minimum_cosigner_index(&all_public_keys, &x_public_keys, prefix)
    };

    let keys = Keys::new(
        keys_file_path.clone(),
        KEY_FILE_VERSION,
        encrypted_mnemonics,
        prefix,
        all_public_keys,
        0,
        0,
        args.min_signatures,
        cosigner_index,
    );

    keys.save().map_err(|e| {
        InternalServerError(format!(
            "Error saving keys file to {}: {}",
            keys_file_path, e
        ))
    })?;

    Ok(keys)
}
fn extract_x_public_keys(
    mnemonics: Arc<Vec<Mnemonic>>,
    is_multisig: bool,
) -> Vec<ExtendedPublicKey<PublicKey>> {
    let master_key_derivation_path = master_key_path(is_multisig);
    let x_private_keys: Vec<ExtendedPrivateKey<SecretKey>> = mnemonics
        .iter()
        .map(|mnemonic: &Mnemonic| {
            let seed = mnemonic.to_seed("");
            let master_key = ExtendedPrivateKey::new(seed).unwrap();
            master_key.derive_path(&master_key_derivation_path).unwrap()
        })
        .collect();
    let x_public_keys: Vec<ExtendedPublicKey<PublicKey>> = x_private_keys
        .iter()
        .map(|x_private_key| x_private_key.public_key())
        .collect();
    x_public_keys
}

fn encrypt_mnemonics(
    password: &String,
    mnemonics: &[Mnemonic],
) -> WalletResult<Vec<EncryptedMnemonic>> {
    let mut encrypted_mnemonics = vec![];
    for mnemonic in mnemonics.iter() {
        let encrypted_mnemonic = EncryptedMnemonic::new(mnemonic, password)
            .map_err(|e| InternalServerError(format!("Error encrypting mnemonics: {}", e)))?;
        encrypted_mnemonics.push(encrypted_mnemonic);
    }
    Ok(encrypted_mnemonics)
}

fn minimum_cosigner_index(
    all_public_keys: &[ExtendedPublicKey<PublicKey>],
    signer_public_keys: &[ExtendedPublicKey<PublicKey>],
    prefix: Prefix,
) -> u16 {
    let mut sorted_public_keys = all_public_keys.to_vec();
    sorted_public_keys.sort_by_key(|a| a.to_string(Some(prefix)));

    let mut minimum_cosigner_index = sorted_public_keys.len();
    for x_public_key in signer_public_keys {
        let current_key_cosigner_index = sorted_public_keys
            .iter()
            .position(|x| x.eq(&x_public_key))
            .unwrap_or(0);
        if current_key_cosigner_index < minimum_cosigner_index {
            minimum_cosigner_index = current_key_cosigner_index;
        }
    }

    minimum_cosigner_index as u16
}
