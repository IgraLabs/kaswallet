use crate::errors::{ResultExt, WalletResult};
use kaspa_addresses::{Address, Prefix, Version};
use kaspa_bip32::secp256k1::PublicKey;
use kaspa_bip32::{DerivationPath, ExtendedPublicKey};
use kaspa_txscript::multisig_redeem_script;
use std::sync::Arc;

pub fn p2pk_address(
    extended_public_key: &ExtendedPublicKey<PublicKey>,
    prefix: Prefix,
    derivation_path: &DerivationPath,
) -> WalletResult<Address> {
    let derived_key = extended_public_key
        .clone()
        .derive_path(derivation_path)
        .to_wallet_result_internal()?;
    let pk = derived_key.public_key();
    let payload = pk.x_only_public_key().0.serialize();
    let address = Address::new(prefix, Version::PubKey, &payload);
    Ok(address)
}

pub fn multisig_address(
    extended_public_keys: Arc<Vec<ExtendedPublicKey<PublicKey>>>,
    minimum_signatures: usize,
    prefix: Prefix,
    derivation_path: &DerivationPath,
) -> WalletResult<Address> {
    let mut sorted_extended_public_keys = extended_public_keys.as_ref().clone();
    sorted_extended_public_keys.sort();

    let mut signing_public_keys = Vec::with_capacity(sorted_extended_public_keys.len());
    for x_public_key in sorted_extended_public_keys.iter() {
        let derived_key = x_public_key
            .clone()
            .derive_path(derivation_path)
            .to_wallet_result_internal()?;
        let public_key = derived_key.public_key();
        signing_public_keys.push(public_key.x_only_public_key().0.serialize());
    }

    let redeem_script = multisig_redeem_script(signing_public_keys.iter(), minimum_signatures)
        .to_wallet_result_internal()?;
    let script_pub_key = kaspa_txscript::pay_to_script_hash_script(redeem_script.as_slice());
    let address = kaspa_txscript::extract_script_pub_key_address(&script_pub_key, prefix)
        .to_wallet_result_internal()?;
    Ok(address)
}
