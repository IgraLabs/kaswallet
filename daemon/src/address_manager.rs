use common::addresses::{multisig_address_from_sorted_keys, p2pk_address};
use common::errors::WalletResult;
use common::keys::Keys;
use common::model::{KEYCHAINS, Keychain, WalletAddress};
use kaspa_addresses::{Address, Prefix as AddressPrefix};
use kaspa_bip32::secp256k1::PublicKey;
use kaspa_bip32::{ChildNumber, DerivationPath, ExtendedPublicKey};
use kaspa_rpc_core::RpcBalancesByAddressesEntry;
use std::collections::HashMap;
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering::Relaxed;
use tokio::sync::Mutex;

pub type AddressSet = HashMap<String, WalletAddress>;
pub type AddressQuerySet = HashMap<Address, WalletAddress>;

#[derive(Debug, Clone)]
struct MonitoredAddressesCache {
    version: u64,
    addresses: Arc<Vec<Address>>,
}

#[derive(Debug)]
pub struct AddressManager {
    keys_file: Arc<Keys>,
    extended_public_keys: Arc<Vec<ExtendedPublicKey<PublicKey>>>,
    addresses: Mutex<AddressSet>,
    is_multisig: bool,
    prefix: AddressPrefix,

    address_cache: Mutex<HashMap<WalletAddress, Address>>,

    address_set_version: AtomicU64,
    monitored_addresses_cache: Mutex<MonitoredAddressesCache>,
}

impl AddressManager {
    pub fn new(keys: Arc<Keys>, prefix: AddressPrefix) -> Self {
        let is_multisig = keys.public_keys.len() > 1;
        let mut sorted_public_keys = keys.public_keys.clone();
        sorted_public_keys.sort();

        Self {
            keys_file: keys.clone(),
            extended_public_keys: Arc::new(sorted_public_keys),
            addresses: Mutex::new(HashMap::new()),
            is_multisig,
            prefix,
            address_cache: Mutex::new(HashMap::new()),
            address_set_version: AtomicU64::new(0),
            monitored_addresses_cache: Mutex::new(MonitoredAddressesCache {
                version: 0,
                addresses: Arc::new(Vec::new()),
            }),
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

    pub async fn monitored_addresses(&self) -> Result<Arc<Vec<Address>>, Box<dyn Error + Send + Sync>> {
        let current_version = self.address_set_version.load(Relaxed);
        {
            let cache = self.monitored_addresses_cache.lock().await;
            if cache.version == current_version {
                return Ok(cache.addresses.clone());
            }
        }

        let addresses_vec: Vec<Address> = {
            let addresses = self.addresses.lock().await;
            let mut parsed = Vec::with_capacity(addresses.len());
            for address_string in addresses.keys() {
                let address = Address::try_from(address_string.as_str()).map_err(|err| {
                    format!("invalid address in wallet address_set ({address_string}): {err}")
                })?;
                parsed.push(address);
            }
            parsed
        };

        let addresses_arc = Arc::new(addresses_vec);
        let mut cache = self.monitored_addresses_cache.lock().await;
        cache.version = current_version;
        cache.addresses = addresses_arc.clone();
        Ok(addresses_arc)
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

        let address_string = address.to_string();
        {
            let mut addresses = self.addresses.lock().await;
            addresses.insert(address_string.clone(), wallet_address.clone());
            self.address_set_version.fetch_add(1, Relaxed);
        }

        Ok((address_string, wallet_address))
    }

    pub async fn addresses_to_query(
        &self,
        start: u32,
        end: u32,
    ) -> Result<AddressQuerySet, Box<dyn Error + Send + Sync>> {
        let mut addresses = HashMap::with_capacity(
            (end.saturating_sub(start) as usize)
                * self.extended_public_keys.len()
                * KEYCHAINS.len(),
        );

        for index in start..end {
            for cosigner_index in 0..self.extended_public_keys.len() as u16 {
                for keychain in KEYCHAINS {
                    let wallet_address = WalletAddress::new(index, cosigner_index, keychain);
                    let address = self
                        .kaspa_address_from_wallet_address(&wallet_address, false)
                        .await?;
                    addresses.insert(address, wallet_address);
                }
            }
        }

        Ok(addresses)
    }

    pub async fn update_addresses_and_last_used_indexes(
        &self,
        mut address_set: AddressQuerySet,
        get_balances_by_addresses_response: Vec<RpcBalancesByAddressesEntry>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        // create scope to release last_used_internal/external_index before keys_file.save() is called
        {
            let mut addresses_guard = self.addresses.lock().await;
            let mut inserted_any = false;
            for entry in get_balances_by_addresses_response {
                if entry.balance == Some(0) {
                    continue;
                }

                let address_string = entry.address.to_string();
                let wallet_address = address_set.remove(&entry.address).unwrap();

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

                addresses_guard.insert(address_string, wallet_address);
                inserted_any = true;
            }
            if inserted_any {
                self.address_set_version.fetch_add(1, Relaxed);
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
        // Avoid string formatting + parsing in the hot address-derivation path.
        let keychain_number = wallet_address.keychain.clone() as u32;

        let mut path = DerivationPath::default();
        if self.is_multisig {
            path.push(ChildNumber(wallet_address.cosigner_index as u32));
        }
        path.push(ChildNumber(keychain_number));
        path.push(ChildNumber(wallet_address.index));
        Ok(path)
    }

    fn p2pk_address(&self, derivation_path: &DerivationPath) -> WalletResult<Address> {
        p2pk_address(
            self.extended_public_keys.first().unwrap(),
            self.prefix,
            derivation_path,
        )
    }

    fn multisig_address(&self, derivation_path: &DerivationPath) -> WalletResult<Address> {
        multisig_address_from_sorted_keys(
            self.extended_public_keys.as_ref(),
            self.keys_file.minimum_signatures as usize,
            self.prefix,
            derivation_path,
        )
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

        let address_string = address.to_string();
        {
            let mut addresses = self.addresses.lock().await;
            addresses.insert(address_string, wallet_address.clone());
            self.address_set_version.fetch_add(1, Relaxed);
        }

        Ok((address, wallet_address))
    }
}

#[cfg(test)]
mod tests {
    use super::{AddressManager, AddressQuerySet, Keychain, WalletAddress};
    use common::keys::Keys;
    use kaspa_addresses::{Address, Prefix, Version};
    use kaspa_bip32::secp256k1::SecretKey;
    use kaspa_bip32::{ExtendedPrivateKey, Language, Mnemonic, Prefix as XPubPrefix};
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_ID: AtomicUsize = AtomicUsize::new(0);

    fn unique_keys_path() -> String {
        let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!("kaswallet-address-manager-test-{id}.json"))
            .to_string_lossy()
            .to_string()
    }

    fn keys_with_no_pubkeys() -> Arc<Keys> {
        Arc::new(Keys::new(
            unique_keys_path(),
            1,
            vec![],
            XPubPrefix::XPUB,
            vec![],
            0,
            0,
            1,
            0,
        ))
    }

    fn keys_with_single_pubkey() -> Arc<Keys> {
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let mnemonic = Mnemonic::new(phrase, Language::English).unwrap();
        let seed = mnemonic.to_seed("");
        let xprv = ExtendedPrivateKey::<SecretKey>::new(seed).unwrap();
        let xprv = xprv
            .derive_path(&common::keys::master_key_path(false))
            .unwrap();
        let xpub = xprv.public_key();

        Arc::new(Keys::new(
            unique_keys_path(),
            1,
            vec![],
            XPubPrefix::XPUB,
            vec![xpub],
            0,
            0,
            1,
            0,
        ))
    }

    #[test]
    fn calculate_address_path_singlesig_matches_expected_format() {
        let manager = AddressManager::new(keys_with_no_pubkeys(), Prefix::Mainnet);
        let wallet_address = WalletAddress::new(7, 0, Keychain::External);

        let path = manager.calculate_address_path(&wallet_address).unwrap();
        assert_eq!(path.to_string(), "m/0/7");
    }

    #[tokio::test]
    async fn addresses_to_query_uses_address_keys_and_expected_count() {
        let manager = AddressManager::new(keys_with_single_pubkey(), Prefix::Devnet);
        let set = manager.addresses_to_query(0, 2).await.unwrap();

        // (end-start)=2 indexes, 1 cosigner, 2 keychains => 4 addresses.
        assert_eq!(set.len(), 4);
    }

    #[tokio::test]
    async fn monitored_addresses_cache_is_reused_and_invalidated_on_change() {
        let keys = keys_with_no_pubkeys();
        let manager = AddressManager::new(keys, Prefix::Mainnet);

        let address1 = Address::new(Prefix::Mainnet, Version::PubKey, &[1u8; 32]);
        let wallet_address1 = WalletAddress::new(1, 0, Keychain::External);

        let mut query_set: AddressQuerySet = HashMap::new();
        query_set.insert(address1.clone(), wallet_address1);

        manager
            .update_addresses_and_last_used_indexes(query_set, vec![kaspa_rpc_core::RpcBalancesByAddressesEntry {
                address: address1.clone(),
                balance: Some(1),
            }])
            .await
            .unwrap();

        let first = manager.monitored_addresses().await.unwrap();
        let second = manager.monitored_addresses().await.unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.len(), 1);

        let address2 = Address::new(Prefix::Mainnet, Version::PubKey, &[2u8; 32]);
        let wallet_address2 = WalletAddress::new(2, 0, Keychain::External);
        let mut query_set: AddressQuerySet = HashMap::new();
        query_set.insert(address2.clone(), wallet_address2);

        manager
            .update_addresses_and_last_used_indexes(query_set, vec![kaspa_rpc_core::RpcBalancesByAddressesEntry {
                address: address2.clone(),
                balance: Some(1),
            }])
            .await
            .unwrap();

        let third = manager.monitored_addresses().await.unwrap();
        assert!(!Arc::ptr_eq(&second, &third));
        assert_eq!(third.len(), 2);
    }
}
