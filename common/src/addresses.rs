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
    multisig_address_from_sorted_keys(
        &sorted_extended_public_keys,
        minimum_signatures,
        prefix,
        derivation_path,
    )
}

pub fn multisig_address_from_sorted_keys(
    sorted_extended_public_keys: &[ExtendedPublicKey<PublicKey>],
    minimum_signatures: usize,
    prefix: Prefix,
    derivation_path: &DerivationPath,
) -> WalletResult<Address> {
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

#[cfg(test)]
mod tests {
    use super::{multisig_address, multisig_address_from_sorted_keys};
    use crate::keys::master_key_path;
    use kaspa_addresses::Prefix;
    use kaspa_bip32::secp256k1::SecretKey;
    use kaspa_bip32::{DerivationPath, ExtendedPrivateKey, Language, Mnemonic};
    use std::sync::Arc;

    fn xpub_from_mnemonic(phrase: &str) -> kaspa_bip32::ExtendedPublicKey<kaspa_bip32::secp256k1::PublicKey> {
        let mnemonic = Mnemonic::new(phrase, Language::English).unwrap();
        let seed = mnemonic.to_seed("");
        let xprv = ExtendedPrivateKey::<SecretKey>::new(seed).unwrap();
        let xprv = xprv.derive_path(&master_key_path(true)).unwrap();
        xprv.public_key()
    }

    #[test]
    fn multisig_address_sorted_helper_matches_existing_function() {
        let xpub1 = xpub_from_mnemonic(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        );
        let xpub2 = xpub_from_mnemonic(
            "legal winner thank year wave sausage worth useful legal winner thank yellow",
        );

        let derivation_path: DerivationPath = "m/0/0/0".parse().unwrap();
        let unsorted = Arc::new(vec![xpub2.clone(), xpub1.clone()]);

        let expected = multisig_address(unsorted, 2, Prefix::Devnet, &derivation_path).unwrap();

        let mut sorted = vec![xpub2, xpub1];
        sorted.sort();
        let actual =
            multisig_address_from_sorted_keys(&sorted, 2, Prefix::Devnet, &derivation_path)
                .unwrap();

        assert_eq!(expected.to_string(), actual.to_string());
    }
}
