use crate::service::kaswallet_service::KasWalletService;
use common::errors::WalletError::UserInputError;
use common::errors::{ResultExt, WalletResult};
use common::model::WalletSignableTransaction;
use kaspa_consensus_core::sign::Signed::{Fully, Partially};
use kaspa_wallet_core::rpc::RpcApi;

impl KasWalletService {
    pub(crate) async fn get_virtual_daa_score(&self) -> WalletResult<u64> {
        let block_dag_info = self
            .kaspa_client
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

    pub(crate) async fn submit_transactions(
        &self,
        signed_transactions: &[WalletSignableTransaction],
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
                .kaspa_client
                .submit_transaction(rpc_transaction, false)
                .await
                .to_wallet_result_internal()?;

            transaction_ids.push(rpc_transaction_id.to_string());

            self.utxo_manager.add_mempool_transaction(signed_transaction).await;
        }

        Ok(transaction_ids)
    }
}
