use crate::address_manager::AddressSet;
use crate::service::kaswallet_service::KasWalletService;
use common::errors::WalletError::UserInputError;
use common::errors::{ResultExt, WalletResult};
use common::model::WalletUtxo;
use kaspa_addresses::Address;
use kaspa_wallet_core::rpc::RpcApi;
use proto::kaswallet_proto::{
    AddressToUtxos, GetUtxosRequest, GetUtxosResponse, Utxo as ProtoUtxo,
};
use std::collections::HashMap;

impl KasWalletService {
    pub(crate) async fn get_utxos(
        &self,
        request: GetUtxosRequest,
    ) -> WalletResult<GetUtxosResponse> {
        let request_addresses = &request.addresses;
        for address in request_addresses {
            Address::try_from(address.as_str()).to_wallet_result_user_input()?;
        }

        let address_set: AddressSet;
        {
            let address_manager = self.address_manager.lock().await;
            address_set = address_manager.address_set().await;
        }
        let address_strings: &Vec<String> = if request_addresses.is_empty() {
            &address_set.keys().cloned().collect()
        } else {
            for address in request_addresses {
                if !address_set.contains_key(address) {
                    return Err(UserInputError(format!(
                        "Address {} not found in wallet",
                        address
                    )));
                }
            }
            request_addresses
        };

        let fee_estimate = self
            .kaspa_client
            .get_fee_estimate()
            .await
            .to_wallet_result_internal()?;

        let fee_rate = fee_estimate.normal_buckets[0].feerate;

        let virtual_daa_score = self.get_virtual_daa_score().await?;

        let filtered_bucketed_utxos: HashMap<String, Vec<ProtoUtxo>>;
        let utxos = {
            let utxo_manager = self.utxo_manager.lock().await;
            utxo_manager.utxos_sorted_by_amount()
        };
        filtered_bucketed_utxos = self
            .filter_utxos_and_bucket_by_address(
                &utxos,
                fee_rate,
                virtual_daa_score,
                address_strings,
                request.include_pending,
                request.include_dust,
            )
            .await?;

        let addresses_to_utxos = filtered_bucketed_utxos
            .iter()
            .map(|(address_string, utxos)| AddressToUtxos {
                address: address_string.clone(),
                utxos: utxos.clone(),
            })
            .collect();

        Ok(GetUtxosResponse { addresses_to_utxos })
    }

    async fn filter_utxos_and_bucket_by_address(
        &self,
        utxos: &Vec<WalletUtxo>,
        fee_rate: f64,
        virtual_daa_score: u64,
        address_strings: &[String],
        include_pending: bool,
        include_dust: bool,
    ) -> WalletResult<HashMap<String, Vec<ProtoUtxo>>> {
        let mut filtered_bucketed_utxos = HashMap::new();
        for utxo in utxos {
            let is_pending = {
                let utxo_manager = self.utxo_manager.lock().await;
                utxo_manager.is_utxo_pending(utxo, virtual_daa_score)
            };
            if !include_pending && is_pending {
                continue;
            }
            let is_dust = self.is_utxo_dust(utxo, fee_rate).await?;
            if !include_dust && is_dust {
                continue;
            }

            let address: String;
            {
                let address_manager = self.address_manager.lock().await;
                address = address_manager
                    .kaspa_address_from_wallet_address(&utxo.address, true)
                    .await?
                    .address_to_string();
            }

            if !address_strings.is_empty() && !address_strings.contains(&address) {
                continue;
            }

            let entry = filtered_bucketed_utxos
                .entry(address)
                .or_insert_with(Vec::new);
            entry.push(utxo.to_owned().into_proto(is_pending, is_dust));
        }

        Ok(filtered_bucketed_utxos)
    }

    async fn is_utxo_dust(&self, utxo: &WalletUtxo, fee_rate: f64) -> WalletResult<bool> {
        let transaction_generator = self.transaction_generator.lock().await;
        let mass = transaction_generator
            .estimate_mass(&vec![utxo.clone()], utxo.utxo_entry.amount, &[])
            .await?;

        let fee = ((mass as f64) * fee_rate).ceil() as u64;

        Ok(fee >= utxo.utxo_entry.amount)
    }
}
