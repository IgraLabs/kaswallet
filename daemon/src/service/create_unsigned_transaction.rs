use crate::service::kaswallet_service::KasWalletService;
use crate::utxo_manager::UtxoManager;
use common::error_location::ErrorLocation;
use common::errors::{UserInputError, WalletError, WalletResult};
use common::model::WalletSignableTransaction;
use proto::kaswallet_proto::{
    CreateUnsignedTransactionsRequest, CreateUnsignedTransactionsResponse, TransactionDescription,
};
use tokio::sync::MutexGuard;

impl KasWalletService {
    pub(crate) async fn create_unsigned_transactions(
        &self,
        request: CreateUnsignedTransactionsRequest,
    ) -> WalletResult<CreateUnsignedTransactionsResponse> {
        if request.transaction_description.is_none() {
            return Err(WalletError::from(UserInputError::MissingField {
                field: "transaction_description",
                loc: ErrorLocation::capture(),
            }));
        }
        let transaction_description = request.transaction_description.unwrap();
        let unsigned_transactions: Vec<WalletSignableTransaction>;
        {
            let utxo_manager = self.utxo_manager.lock().await;
            unsigned_transactions = self
                .create_unsigned_transactions_from_description(
                    transaction_description,
                    &utxo_manager,
                )
                .await?;
        }

        Ok(CreateUnsignedTransactionsResponse {
            unsigned_transactions: unsigned_transactions.into_iter().map(Into::into).collect(),
        })
    }

    pub(crate) async fn create_unsigned_transactions_from_description(
        &self,
        transaction_description: TransactionDescription,
        utxo_manager: &MutexGuard<'_, UtxoManager>,
    ) -> WalletResult<Vec<WalletSignableTransaction>> {
        self.check_is_synced().await?;

        let mut transaction_generator = self.transaction_generator.lock().await;
        transaction_generator
            .create_unsigned_transactions(utxo_manager, transaction_description)
            .await
    }
}
