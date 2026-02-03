use crate::service::kaswallet_service::KasWalletService;
use common::errors::WalletError::{InternalServerError, UserInputError};
use common::errors::WalletResult;
use common::model::WalletSignableTransaction;
use proto::kaswallet_proto::{
    CreateUnsignedTransactionsRequest, CreateUnsignedTransactionsResponse, TransactionDescription,
};
use crate::utxo_manager::UtxoStateView;

impl KasWalletService {
    pub(crate) async fn create_unsigned_transactions(
        &self,
        request: CreateUnsignedTransactionsRequest,
    ) -> WalletResult<CreateUnsignedTransactionsResponse> {
        if request.transaction_description.is_none() {
            return Err(UserInputError(
                "Transaction description is required".to_string(),
            ));
        }
        let transaction_description = request.transaction_description.unwrap();
        let unsinged_transactions: Vec<WalletSignableTransaction>;
        {
            let utxo_state = self
                .utxo_manager
                .state_with_mempool()
                .await
                .map_err(|e| InternalServerError(e.to_string()))?;
            unsinged_transactions = self
                .create_unsigned_transactions_from_description(
                    transaction_description,
                    &utxo_state,
                )
                .await?;
        }

        Ok(CreateUnsignedTransactionsResponse {
            unsigned_transactions: unsinged_transactions.into_iter().map(Into::into).collect(),
        })
    }

    pub(crate) async fn create_unsigned_transactions_from_description(
        &self,
        transaction_description: TransactionDescription,
        utxo_state: &UtxoStateView,
    ) -> WalletResult<Vec<WalletSignableTransaction>> {
        self.check_is_synced().await?;

        let mut transaction_generator = self.transaction_generator.lock().await;
        transaction_generator
            .create_unsigned_transactions(
                self.utxo_manager.as_ref(),
                utxo_state,
                transaction_description,
            )
            .await
    }
}
