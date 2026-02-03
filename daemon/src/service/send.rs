use crate::service::kaswallet_service::KasWalletService;
use common::errors::WalletError::{InternalServerError, UserInputError};
use common::errors::WalletResult;
use log::{debug, error, info};
use proto::kaswallet_proto::{SendRequest, SendResponse};
use std::time::Instant;

impl KasWalletService {
    pub(crate) async fn send(&self, request: SendRequest) -> WalletResult<SendResponse> {
        let send_start = Instant::now();
        let transaction_description = match request.transaction_description {
            Some(description) => description,
            None => {
                return Err(UserInputError(
                    "Transaction description is required".to_string(),
                ));
            }
        };
        debug!(
            "Got a request for transaction: {:?}",
            transaction_description
        );

        debug!("Creating unsigned transactions...");

        let utxo_state = self
            .utxo_manager
            .state_with_mempool()
            .await
            .map_err(|e| InternalServerError(e.to_string()))?;
        let unsigned_transactions = self
            .create_unsigned_transactions_from_description(transaction_description, &utxo_state)
            .await?;
        debug!("Created {} transactions", unsigned_transactions.len());

        debug!("Signing transactions...");
        let signed_transactions = self
            .sign_transactions(unsigned_transactions, &request.password)
            .await?;
        debug!("Transactions got signed!");

        debug!("Submitting transactions...");
        let submit_transactions_result = self.submit_transactions(&signed_transactions).await;
        if let Err(e) = submit_transactions_result {
            error!("Failed to submit transactions: {}", e);
            return Err(e);
        }
        let transaction_ids = submit_transactions_result?;
        debug!("Transactions submitted: {:?}", transaction_ids);

        info!(
            "Total time to serve send request: {:?}",
            send_start.elapsed()
        );
        Ok(SendResponse {
            transaction_ids,
            signed_transactions: signed_transactions.into_iter().map(Into::into).collect(),
        })
    }
}
