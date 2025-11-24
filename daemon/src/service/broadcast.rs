use crate::service::kaswallet_service::KasWalletService;
use common::errors::WalletResult;
use proto::kaswallet_proto::{BroadcastRequest, BroadcastResponse};

impl KasWalletService {
    pub(crate) async fn broadcast(
        &self,
        request: BroadcastRequest,
    ) -> WalletResult<BroadcastResponse> {
        let signed_transactions: Vec<_> =
            request.transactions.into_iter().map(Into::into).collect();

        let mut utxo_manager = self.utxo_manager.lock().await;
        let transaction_ids = self
            .submit_transactions(&mut utxo_manager, &signed_transactions)
            .await?;

        Ok(BroadcastResponse { transaction_ids })
    }
}
