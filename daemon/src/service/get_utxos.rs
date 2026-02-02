use crate::address_manager::AddressSet;
use crate::service::kaswallet_service::KasWalletService;
use common::errors::WalletError::{InternalServerError, UserInputError};
use common::errors::{ResultExt, WalletResult};
use common::model::WalletAddress;
use kaspa_addresses::Address;
use kaspa_wallet_core::rpc::RpcApi;
use proto::kaswallet_proto::{
    AddressToUtxos, GetUtxosRequest, GetUtxosResponse, Utxo as ProtoUtxo,
};
use std::collections::{HashMap, HashSet};

impl KasWalletService {
    pub(crate) async fn get_utxos(
        &self,
        request: GetUtxosRequest,
    ) -> WalletResult<GetUtxosResponse> {
        for address in &request.addresses {
            Address::try_from(address.as_str()).to_wallet_result_user_input()?;
        }

        let address_set: AddressSet;
        {
            let address_manager = self.address_manager.lock().await;
            address_set = address_manager.address_set().await;
        }
        let allowed_addresses: Option<HashSet<String>> = if request.addresses.is_empty() {
            None
        } else {
            for address in &request.addresses {
                if !address_set.contains_key(address) {
                    return Err(UserInputError(format!(
                        "Address {} not found in wallet",
                        address
                    )));
                }
            }
            Some(request.addresses.iter().cloned().collect())
        };
        let wallet_address_to_string: HashMap<WalletAddress, String> = address_set
            .iter()
            .map(|(address_string, wallet_address)| {
                (wallet_address.clone(), address_string.clone())
            })
            .collect();

        let fee_estimate = self
            .kaspa_client
            .get_fee_estimate()
            .await
            .to_wallet_result_internal()?;

        let fee_rate = fee_estimate.normal_buckets[0].feerate;

        let virtual_daa_score = self.get_virtual_daa_score().await?;

        let dust_fee_for_single_utxo = if request.include_dust {
            None
        } else {
            let sample_utxo = {
                let utxo_manager = self.utxo_manager.lock().await;
                utxo_manager.utxos_sorted_by_amount().next().cloned()
            };
            if let Some(sample_utxo) = sample_utxo {
                let transaction_generator = self.transaction_generator.lock().await;
                let mass = transaction_generator
                    .estimate_mass(&vec![sample_utxo.clone()], sample_utxo.utxo_entry.amount, &[])
                    .await?;
                Some(((mass as f64) * fee_rate).ceil() as u64)
            } else {
                None
            }
        };

        let mut filtered_bucketed_utxos: HashMap<String, Vec<ProtoUtxo>> = HashMap::new();
        {
            let utxo_manager = self.utxo_manager.lock().await;
            for utxo in utxo_manager.utxos_sorted_by_amount() {
                let is_pending = utxo_manager.is_utxo_pending(utxo, virtual_daa_score);
                if !request.include_pending && is_pending {
                    continue;
                }

                let is_dust = dust_fee_for_single_utxo
                    .is_some_and(|dust_fee| dust_fee >= utxo.utxo_entry.amount);
                if !request.include_dust && is_dust {
                    continue;
                }

                let address = wallet_address_to_string.get(&utxo.address).ok_or_else(|| {
                    InternalServerError(format!(
                        "wallet address missing from address_set: {:?}",
                        utxo.address
                    ))
                })?;
                if let Some(allowed_addresses) = &allowed_addresses {
                    if !allowed_addresses.contains(address) {
                        continue;
                    }
                }

                match filtered_bucketed_utxos.get_mut(address) {
                    Some(bucket) => bucket.push(utxo.to_proto(is_pending, is_dust)),
                    None => {
                        filtered_bucketed_utxos.insert(
                            address.clone(),
                            vec![utxo.to_proto(is_pending, is_dust)],
                        );
                    }
                }
            }
        }

        let addresses_to_utxos = filtered_bucketed_utxos
            .into_iter()
            .map(|(address, utxos)| AddressToUtxos { address, utxos })
            .collect();

        Ok(GetUtxosResponse { addresses_to_utxos })
    }
}
