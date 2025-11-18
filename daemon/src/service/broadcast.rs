use crate::service::kaswallet_service::KasWalletService;
use common::errors::WalletResult;
use common::transactions_encoding::decode_transactions;
use proto::kaswallet_proto::{BroadcastRequest, BroadcastResponse};

impl KasWalletService {
    pub(crate) async fn broadcast(
        &self,
        request: BroadcastRequest,
    ) -> WalletResult<BroadcastResponse> {
        let encoded_signed_transactions = &request.transactions;
        let signed_transactions = decode_transactions(encoded_signed_transactions)?;

        let mut utxo_manager = self.utxo_manager.lock().await;
        let transaction_ids = self
            .submit_transactions(&mut utxo_manager, &signed_transactions)
            .await?;

        Ok(BroadcastResponse { transaction_ids })
    }
}
