use common::errors::{ResultExt, WalletResult};
use common::keys::Keys;
use common::model::{Keychain, WalletAddress, KEYCHAINS};
use kaspa_addresses::{Address, Prefix as AddressPrefix, Version as AddressVersion};
use kaspa_bip32::secp256k1::PublicKey;
use kaspa_bip32::{DerivationPath, ExtendedPublicKey};
use kaspa_rpc_core::RpcBalancesByAddressesEntry;
use std::collections::HashMap;
use std::error::Error;
use std::str::FromStr;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use tokio::sync::Mutex;

pub type AddressSet = HashMap<String, WalletAddress>;
#[derive(Debug)]
pub struct AddressManager {
    keys_file: Arc<Keys>,
    extended_public_keys: Arc<Vec<ExtendedPublicKey<PublicKey>>>,
    addresses: Mutex<AddressSet>,
    is_multisig: bool,
    prefix: AddressPrefix,

    address_cache: Mutex<HashMap<WalletAddress, Address>>,
}

impl AddressManager {
    pub fn new(keys: Arc<Keys>, prefix: AddressPrefix) -> Self {
        let is_multisig = keys.public_keys.len() > 1;

        Self {
            keys_file: keys.clone(),
            extended_public_keys: Arc::new(keys.public_keys.clone()),
            addresses: Mutex::new(HashMap::new()),
            is_multisig,
            prefix,
            address_cache: Mutex::new(HashMap::new()),
        }
    }

    pub async fn wallet_address_from_string(&self, address_string: &str) -> Option<WalletAddress> {
        let addresses = self.addresses.lock().await;
        let address = addresses.get(address_string);
        address.cloned()
    }

    pub async fn address_set(&self) -> AddressSet {
        let addresses = self.addresses.lock().await;
        addresses.clone()
    }

    pub async fn address_strings(&self) -> Result<Vec<String>, Box<dyn Error + Send + Sync>> {
        let addresses = self.addresses.lock().await;
        let strings = addresses
            .keys()
            .map(|address_string| address_string.to_string())
            .collect();

        Ok(strings)
    }

    pub async fn new_address(&self) -> WalletResult<(String, WalletAddress)> {
        let last_used_external_index_previous_value = self
            .keys_file
            .last_used_external_index
            .fetch_add(1, Relaxed);
        let last_used_external_index = last_used_external_index_previous_value + 1;
        self.keys_file.save()?;

        let wallet_address = WalletAddress::new(
            last_used_external_index,
            self.keys_file.cosigner_index,
            Keychain::External,
        );
        let address = self
            .kaspa_address_from_wallet_address(&wallet_address, true)
            .await?;

        Ok((address.to_string(), wallet_address))
    }

    pub async fn addresses_to_query(
        &self,
        start: u32,
        end: u32,
    ) -> Result<AddressSet, Box<dyn Error + Send + Sync>> {
        let mut addresses = HashMap::new();

        for index in start..end {
            for cosigner_index in 0..self.extended_public_keys.len() as u16 {
                for keychain in KEYCHAINS {
                    let wallet_address = WalletAddress::new(index, cosigner_index, keychain);
                    let address = self
                        .kaspa_address_from_wallet_address(&wallet_address, false)
                        .await?;
                    addresses.insert(address.to_string(), wallet_address);
                }
            }
        }

        Ok(addresses)
    }

    pub async fn update_addresses_and_last_used_indexes(
        &self,
        mut address_set: AddressSet,
        get_balances_by_addresses_response: Vec<RpcBalancesByAddressesEntry>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        // create scope to release last_used_internal/external_index before keys_file.save() is called
        {
            for entry in get_balances_by_addresses_response {
                if entry.balance == Some(0) {
                    continue;
                }

                let address_string = entry.address.to_string();
                let wallet_address = address_set.remove(&address_string).unwrap();

                if wallet_address.keychain == Keychain::External {
                    if wallet_address.index > self.keys_file.last_used_external_index.load(Relaxed)
                    {
                        self.keys_file
                            .last_used_external_index
                            .store(wallet_address.index, Relaxed);
                    }
                } else if wallet_address.index
                    > self.keys_file.last_used_internal_index.load(Relaxed)
                {
                    self.keys_file
                        .last_used_internal_index
                        .store(wallet_address.index, Relaxed);
                }

                self.addresses
                    .lock()
                    .await
                    .insert(address_string, wallet_address);
            }
        }

        self.keys_file.save()?;

        Ok(())
    }

    pub async fn kaspa_address_from_wallet_address(
        &self,
        wallet_address: &WalletAddress,
        should_cache: bool,
    ) -> WalletResult<Address> {
        {
            let address_cache = self.address_cache.lock().await;
            if let Some(address) = address_cache.get(wallet_address) {
                return Ok(address.clone());
            }
        }
        let path = self.calculate_address_path(wallet_address)?;

        let address = self
            .kaspa_address_from_path(wallet_address, &path, should_cache)
            .await?;

        Ok(address)
    }

    async fn kaspa_address_from_path(
        &self,
        wallet_address: &WalletAddress,
        path: &DerivationPath,
        should_cache: bool,
    ) -> WalletResult<Address> {
        let address = if self.is_multisig {
            self.multisig_address(path)?
        } else {
            self.p2pk_address(path)?
        };

        if should_cache {
            let mut address_cache = self.address_cache.lock().await;
            address_cache.insert(wallet_address.clone(), address.clone());
        }
        Ok(address)
    }

    pub fn calculate_address_path(
        &self,
        wallet_address: &WalletAddress,
    ) -> WalletResult<DerivationPath> {
        let keychain_number = wallet_address.keychain.clone() as u32;
        let path_string = if self.is_multisig {
            format!(
                "m/{}/{}/{}",
                wallet_address.cosigner_index, keychain_number, wallet_address.index
            )
        } else {
            format!("m/{}/{}", keychain_number, wallet_address.index)
        };

        let path = DerivationPath::from_str(&path_string).to_wallet_result_internal()?;
        Ok(path)
    }

    fn p2pk_address(&self, derivation_path: &DerivationPath) -> WalletResult<Address> {
        let extended_public_key = self.extended_public_keys.first().unwrap().clone();
        let derived_key = extended_public_key
            .derive_path(derivation_path)
            .to_wallet_result_internal()?;
        let pk = derived_key.public_key();
        let payload = pk.x_only_public_key().0.serialize();
        let address = Address::new(self.prefix, AddressVersion::PubKey, &payload);
        Ok(address)
    }

    fn multisig_address(&self, derivation_path: &DerivationPath) -> WalletResult<Address> {
        let mut sorted_extended_public_keys = self.extended_public_keys.as_ref().clone();
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

        let redeem_script = kaspa_txscript::multisig_redeem_script(
            signing_public_keys.iter(),
            self.keys_file.minimum_signatures as usize,
        )
            .to_wallet_result_internal()?;
        let script_pub_key = kaspa_txscript::pay_to_script_hash_script(redeem_script.as_slice());
        let address = kaspa_txscript::extract_script_pub_key_address(&script_pub_key, self.prefix)
            .to_wallet_result_internal()?;
        Ok(address)
    }

    pub async fn change_address(
        &self,
        use_existing_change_address: bool,
        from_addresses: &[&WalletAddress],
    ) -> WalletResult<(Address, WalletAddress)> {
        let wallet_address = if !from_addresses.is_empty() {
            from_addresses[0].clone()
        } else {
            let internal_index = if use_existing_change_address {
                0
            } else {
                self.keys_file
                    .last_used_internal_index
                    .fetch_add(1, Relaxed)
                    + 1
            };
            self.keys_file.save()?;

            WalletAddress::new(
                internal_index,
                self.keys_file.cosigner_index,
                Keychain::Internal,
            )
        };

        let address = self
            .kaspa_address_from_wallet_address(&wallet_address, true)
            .await?;

        Ok((address, wallet_address))
    }
}
