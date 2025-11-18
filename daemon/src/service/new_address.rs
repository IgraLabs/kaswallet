use crate::service::kaswallet_service::KasWalletService;
use common::errors::WalletResult;
use proto::kaswallet_proto::{NewAddressRequest, NewAddressResponse};

impl KasWalletService {
    pub(crate) async fn new_address(
        &self,
        _request: NewAddressRequest,
    ) -> WalletResult<NewAddressResponse> {
        self.check_is_synced().await?;

        let address_manager = self.address_manager.lock().await;

        let (address_string, _) = address_manager.new_address().await?;

        Ok(NewAddressResponse {
            address: address_string,
        })
    }
}
