use crate::service::kaswallet_service::KasWalletService;
use crate::utxo_manager::UtxoManager;
use common::errors::WalletError::UserInputError;
use common::errors::WalletResult;
use common::model::WalletSignableTransaction;
use common::transactions_encoding::encode_transactions;
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
            return Err(UserInputError(
                "Transaction description is required".to_string(),
            ));
        }
        let transaction_description = request.transaction_description.unwrap();
        let unsinged_transactions: Vec<WalletSignableTransaction>;
        {
            let utxo_manager = self.utxo_manager.lock().await;
            unsinged_transactions = self
                .create_unsigned_transactions_from_description(
                    transaction_description,
                    &utxo_manager,
                )
                .await?;
        }

        let encoded_transactions = encode_transactions(&unsinged_transactions)?;
        Ok(CreateUnsignedTransactionsResponse {
            unsigned_transactions: encoded_transactions,
        })
    }

    pub(crate) async fn create_unsigned_transactions_from_description(
        &self,
        transaction_description: TransactionDescription,
        utxo_manager: &MutexGuard<'_, UtxoManager>,
    ) -> WalletResult<Vec<WalletSignableTransaction>> {
        // TODO: implement manual utxo selection
        if !transaction_description.utxos.is_empty() {
            return Err(UserInputError("UTXOs are not supported yet".to_string()));
        }

        self.check_is_synced().await?;

        let mut transaction_generator = self.transaction_generator.lock().await;
        transaction_generator
            .create_unsigned_transactions(
                utxo_manager,
                transaction_description.to_address,
                transaction_description.amount,
                transaction_description.is_send_all,
                transaction_description.payload.to_vec(),
                transaction_description.from_addresses,
                transaction_description.utxos,
                transaction_description.use_existing_change_address,
                transaction_description.fee_policy,
            )
            .await
    }
}
