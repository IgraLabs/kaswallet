use crate::service::service::KasWalletService;
use common::errors::WalletError::InternalServerError;
use common::errors::WalletResult;
use common::model::{Keychain, WalletAddress};
use kaswallet_proto::kaswallet_proto::GetAddressesRequest;
use std::sync::atomic::Ordering::Relaxed;

impl KasWalletService {
    pub(crate) async fn get_addresses(
        &self,
        _request: GetAddressesRequest,
    ) -> WalletResult<Vec<String>> {
        self.check_is_synced().await?;

        let mut addresses = vec![];
        let address_manager = self.address_manager.lock().await;
        for i in 1..=self.keys.last_used_external_index.load(Relaxed) {
            let wallet_address = WalletAddress {
                index: i,
                cosigner_index: self.keys.cosigner_index,
                keychain: Keychain::External,
            };
            match address_manager
                .kaspa_address_from_wallet_address(&wallet_address, true)
                .await
            {
                Ok(address) => {
                    addresses.push(address.to_string());
                }
                Err(e) => {
                    return Err(InternalServerError(format!(
                        "Failed to calculate address: {}",
                        e
                    )));
                }
            }
        }

        Ok(addresses)
    }
}
