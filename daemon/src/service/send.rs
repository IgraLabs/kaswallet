use crate::service::service::KasWalletService;
use common::errors::WalletError::UserInputError;
use common::errors::WalletResult;
use common::transactions_encoding::encode_transactions;
use proto::kaswallet_proto::{SendRequest, SendResponse};
use log::{debug, error};
use std::time::Instant;

impl KasWalletService {
    pub(crate) async fn send(&self, request: SendRequest) -> WalletResult<SendResponse> {
        // lock utxo_manager at this point, so that if sync happens in the middle - it doesn't
        // interfere with apply_transaction
        let mut utxo_manager = self.utxo_manager.lock().await;

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

        let unsigned_transactions = self
            .create_unsigned_transactions_from_description(transaction_description, &utxo_manager)
            .await?;
        debug!("Created {} transactions", unsigned_transactions.len());

        debug!("Signing transactions...");
        let signed_transactions = self
            .sign_transactions(unsigned_transactions, &request.password)
            .await?;
        debug!("Transactions got signed!");

        debug!("Submitting transactions...");
        let submit_transactions_result = self
            .submit_transactions(&mut utxo_manager, &signed_transactions)
            .await;
        if let Err(e) = submit_transactions_result {
            error!("Failed to submit transactions: {}", e);
            return Err(e);
        }
        let transaction_ids = submit_transactions_result?;
        debug!("Transactions submitted: {:?}", transaction_ids);
        let encoded_signed_transactions = encode_transactions(&signed_transactions)?;

        println!(
            "Total time to serve send request: {:?}",
            send_start.elapsed()
        );
        Ok(SendResponse {
            transaction_ids,
            signed_transactions: encoded_signed_transactions,
        })
    }
}
