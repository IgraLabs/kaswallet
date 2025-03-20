use crate::model::{Keychain, WalletAddress, KEYCHAINS};
use common::keys::Keys;
use kaspa_addresses::{Address, Prefix as AddressPrefix, Version as AddressVersion};
use kaspa_bip32::secp256k1::PublicKey;
use kaspa_bip32::{DerivationPath, ExtendedPublicKey};
use kaspa_wrpc_client::prelude::*;
use kaspa_wrpc_client::KaspaRpcClient;
use log::{debug, info};
use std::collections::HashMap;
use std::error::Error;
use std::str::FromStr;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use tokio::sync::Mutex;

const NUM_INDEXES_TO_QUERY_FOR_FAR_ADDRESSES: u32 = 100;
const NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES: u32 = 1000;

pub type AddressSet = HashMap<String, WalletAddress>;
#[derive(Debug)]
pub struct AddressManager {
    kaspa_rpc_client: Arc<KaspaRpcClient>,

    keys_file: Arc<Keys>,
    extended_public_keys: Arc<Vec<ExtendedPublicKey<PublicKey>>>,
    addresses: Mutex<AddressSet>,
    next_sync_start_index: Mutex<u32>,
    is_multisig: bool,
    prefix: AddressPrefix,

    is_log_final_progress_line_shown: bool,
    max_used_addresses_for_log: u32,
    max_processed_addresses_for_log: u32,
}

impl AddressManager {
    pub fn new(
        kaspa_rpc_client: Arc<KaspaRpcClient>,
        keys: Arc<Keys>,
        prefix: AddressPrefix,
    ) -> Self {
        let is_multisig = keys.public_keys.len() > 0;

        Self {
            kaspa_rpc_client,
            keys_file: keys.clone(),
            extended_public_keys: Arc::new(keys.public_keys.clone()),
            addresses: Mutex::new(HashMap::new()),
            next_sync_start_index: Mutex::new(0),
            is_multisig,
            prefix,
            is_log_final_progress_line_shown: false,
            max_used_addresses_for_log: 0,
            max_processed_addresses_for_log: 0,
        }
    }

    pub async fn is_synced(&self) -> bool {
        *self.next_sync_start_index.lock().await > self.last_used_index().await
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

    pub async fn new_address(
        &self,
    ) -> Result<(String, WalletAddress), Box<dyn Error + Send + Sync>> {
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
        let address = self.calculate_address(&wallet_address)?;

        Ok((address.to_string(), wallet_address))
    }

    pub async fn collect_recent_addresses(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        debug!("Collecting recent addresses");

        let mut index: u32 = 0;
        let mut max_used_index: u32 = 0;

        while index < max_used_index + NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES {
            let collect_addresses_result = self
                .collect_addresses(index, index + NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES)
                .await;
            if let Err(e) = collect_addresses_result {
                return Err(e);
            }
            index += NUM_INDEXES_TO_QUERY_FOR_RECENT_ADDRESSES;

            max_used_index = self.last_used_index().await;

            self.update_address_collection_progress_log(index, max_used_index);
        }

        let mut next_sync_start_index = self.next_sync_start_index.lock().await;
        if index > *next_sync_start_index {
            *next_sync_start_index = index;
        }
        Ok(())
    }

    pub async fn collect_far_addresses(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        debug!("Collecting far addresses");

        let mut next_sync_start_index = self.next_sync_start_index.lock().await;

        self.collect_addresses(
            *next_sync_start_index,
            *next_sync_start_index + NUM_INDEXES_TO_QUERY_FOR_FAR_ADDRESSES,
        )
        .await?;

        *next_sync_start_index += NUM_INDEXES_TO_QUERY_FOR_FAR_ADDRESSES;

        Ok(())
    }

    async fn collect_addresses(
        &self,
        start: u32,
        end: u32,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        debug!("Collecting addresses from {} to {}", start, end);

        let addresses = self.addresses_to_query(start, end)?;

        let get_balances_by_addresses_response = self
            .kaspa_rpc_client
            .get_balances_by_addresses(
                addresses
                    .iter()
                    .map(|(address_string, _)| Address::constructor(address_string))
                    .collect(),
            )
            .await?;

        self.update_addresses_and_last_used_indexes(addresses, get_balances_by_addresses_response)
            .await?;

        Ok(())
    }

    fn addresses_to_query(
        &self,
        start: u32,
        end: u32,
    ) -> Result<AddressSet, Box<dyn Error + Send + Sync>> {
        let mut addresses = HashMap::new();

        for index in start..end {
            for cosigner_index in 0..self.extended_public_keys.len() as u16 {
                for keychain in KEYCHAINS {
                    let wallet_address = WalletAddress::new(index, cosigner_index, keychain);
                    let address = self.calculate_address(&wallet_address)?;
                    addresses.insert(address.to_string(), wallet_address);
                }
            }
        }

        Ok(addresses)
    }

    async fn update_addresses_and_last_used_indexes(
        &self,
        address_set: AddressSet,
        get_balances_by_addresses_response: Vec<RpcBalancesByAddressesEntry>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        // create scope to release last_used_internal/external_index before keys_file.save() is called
        {
            for entry in get_balances_by_addresses_response {
                if entry.balance == Some(0) {
                    continue;
                }

                let address_string = entry.address.to_string();
                let wallet_address = address_set.get(&address_string).unwrap();

                self.addresses
                    .lock()
                    .await
                    .insert(address_string, wallet_address.clone());

                if wallet_address.keychain == Keychain::External {
                    if wallet_address.index > self.keys_file.last_used_external_index.load(Relaxed)
                    {
                        self.keys_file
                            .last_used_external_index
                            .store(wallet_address.index, Relaxed);
                    }
                } else {
                    if wallet_address.index > self.keys_file.last_used_internal_index.load(Relaxed)
                    {
                        self.keys_file
                            .last_used_internal_index
                            .store(wallet_address.index, Relaxed);
                    }
                }
            }
        }

        self.keys_file.save()?;

        Ok(())
    }

    pub fn calculate_address(
        &self,
        wallet_address: &WalletAddress,
    ) -> Result<Address, Box<dyn Error + Send + Sync>> {
        let path = self.calculate_address_path(wallet_address)?;

        if self.is_multisig {
            self.p2pk_address(path)
        } else {
            self.multisig_address(path)
        }
    }

    pub fn calculate_address_path(
        &self,
        wallet_address: &WalletAddress,
    ) -> Result<DerivationPath, Box<dyn Error + Send + Sync>> {
        let wallet_address = wallet_address.clone();
        let path_string = if self.is_multisig {
            format!(
                "m/{}/{}/{}",
                wallet_address.cosigner_index, wallet_address.keychain as u32, wallet_address.index
            )
        } else {
            format!(
                "m/{}/{}",
                wallet_address.keychain as u32, wallet_address.index
            )
        };

        let path = DerivationPath::from_str(&path_string)?;
        Ok(path)
    }

    fn p2pk_address(
        &self,
        derivation_path: DerivationPath,
    ) -> Result<Address, Box<dyn Error + Send + Sync>> {
        let extended_public_key = self.extended_public_keys.first().unwrap().clone();
        let derived_key = extended_public_key.derive_path(&derivation_path)?;
        let pk = derived_key.public_key();
        let payload = pk.x_only_public_key().0.serialize();
        let address = Address::new(self.prefix, AddressVersion::PubKey, &payload);
        Ok(address)
    }

    fn multisig_address(
        &self,
        derivation_path: DerivationPath,
    ) -> Result<Address, Box<dyn Error + Send + Sync>> {
        let mut sorted_extended_public_keys = self.extended_public_keys.as_ref().clone();
        sorted_extended_public_keys.sort();

        let mut public_keys = vec![];
        for x_public_key in sorted_extended_public_keys.iter() {
            let derived_key = x_public_key.clone().derive_path(&derivation_path)?;
            let public_key = derived_key.public_key();
            public_keys.push(public_key.x_only_public_key().0.serialize());
        }

        let redeem_script = kaspa_txscript::multisig_redeem_script(
            public_keys.iter(),
            self.keys_file.minimum_signatures as usize,
        )?;
        let script_pub_key = kaspa_txscript::pay_to_script_hash_script(redeem_script.as_slice());
        let address = kaspa_txscript::extract_script_pub_key_address(&script_pub_key, self.prefix)?;
        Ok(address)
    }

    async fn last_used_index(&self) -> u32 {
        let last_used_external_index = self.keys_file.last_used_external_index.load(Relaxed);
        let last_used_internal_index = self.keys_file.last_used_internal_index.load(Relaxed);

        if last_used_external_index > last_used_internal_index {
            last_used_external_index
        } else {
            last_used_internal_index
        }
    }

    fn update_address_collection_progress_log(
        &mut self,
        processed_addresses: u32,
        max_used_addresses: u32,
    ) {
        if max_used_addresses > self.max_used_addresses_for_log {
            self.max_used_addresses_for_log = max_used_addresses;
            if self.is_log_final_progress_line_shown {
                info!("An additional set of previously used addresses found, processing...");
                self.max_processed_addresses_for_log = 0;
                self.is_log_final_progress_line_shown = false;
            }
        }

        if processed_addresses > self.max_processed_addresses_for_log {
            self.max_processed_addresses_for_log = processed_addresses
        }

        if self.max_processed_addresses_for_log >= self.max_used_addresses_for_log {
            if !self.is_log_final_progress_line_shown {
                info!("Finished scanning recent addresses");
                self.is_log_final_progress_line_shown = true;
            }
        } else {
            let percent_processed = self.max_processed_addresses_for_log as f64
                / self.max_used_addresses_for_log as f64
                * 100.0;

            info!(
                "{} addressed of {} of processed ({:.2})",
                self.max_processed_addresses_for_log,
                self.max_used_addresses_for_log,
                percent_processed
            );
        }
    }

    pub fn change_address(
        &self,
        use_existing_change_address: bool,
        from_addresses: &Vec<&WalletAddress>,
    ) -> Result<(Address, WalletAddress), Box<dyn Error + Send + Sync>> {
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

        let address = self.calculate_address(&wallet_address)?;

        Ok((address, wallet_address))
    }
}
