use crate::model::WalletSignableTransaction;
use crate::service::service::KasWalletService;
use crate::utxo_manager::UtxoManager;
use common::errors::WalletError::UserInputError;
use common::errors::{ResultExt, WalletResult};
use kaspa_consensus_core::sign::Signed::{Fully, Partially};
use kaspa_wallet_core::rpc::RpcApi;
use tokio::sync::MutexGuard;

impl KasWalletService {
    pub(crate) async fn get_virtual_daa_score(&self) -> WalletResult<u64> {
        let block_dag_info = self
            .kaspa_rpc_client
            .get_block_dag_info()
            .await
            .to_wallet_result_internal()?;

        Ok(block_dag_info.virtual_daa_score)
    }

    pub(crate) async fn check_is_synced(&self) -> WalletResult<()> {
        if !self.sync_manager.is_synced().await {
            Err(UserInputError(
                "Wallet is not synced yet. Please wait for the sync to complete.".to_string(),
            ))
        } else {
            Ok(())
        }
    }

    pub(crate) fn decode_transactions(
        encoded_transactions: &Vec<Vec<u8>>,
    ) -> WalletResult<Vec<WalletSignableTransaction>> {
        let mut unsigned_transactions = vec![];
        for encoded_transaction_transaction in encoded_transactions {
            let unsigned_transaction = borsh::from_slice(&encoded_transaction_transaction)
                .to_wallet_result_user_input()?;
            unsigned_transactions.push(unsigned_transaction);
        }
        Ok(unsigned_transactions)
    }

    pub(crate) fn encode_transactions(
        transactions: &Vec<WalletSignableTransaction>,
    ) -> WalletResult<Vec<Vec<u8>>> {
        let mut encoded_transactions = Vec::with_capacity(transactions.len());
        for unsigned_transaction in transactions {
            // TODO: Use protobuf instead of borsh for serialization
            let encoded_transaction =
                borsh::to_vec(&unsigned_transaction).to_wallet_result_internal()?;
            encoded_transactions.push(encoded_transaction);
        }
        Ok(encoded_transactions)
    }

    pub(crate) async fn submit_transactions(
        &self,
        utxo_manager: &mut MutexGuard<'_, UtxoManager>,
        signed_transactions: &Vec<WalletSignableTransaction>,
    ) -> WalletResult<Vec<String>> {
        let _ = self.submit_transaction_mutex.lock().await;

        let mut transaction_ids = vec![];
        for signed_transaction in signed_transactions {
            if let Partially(_) = signed_transaction.transaction {
                return Err(UserInputError(
                    "Transaction is not fully signed".to_string(),
                ));
            }

            let tx = match &signed_transaction.transaction {
                Fully(tx) => tx,
                Partially(tx) => tx,
            };
            let rpc_transaction = (&tx.tx).into();

            let rpc_transaction_id = self
                .kaspa_rpc_client
                .submit_transaction(rpc_transaction, false)
                .await
                .to_wallet_result_internal()?;

            transaction_ids.push(rpc_transaction_id.to_string());

            for transaction in signed_transactions {
                utxo_manager.add_mempool_transaction(transaction).await;
            }
        }

        Ok(transaction_ids)
    }
}
