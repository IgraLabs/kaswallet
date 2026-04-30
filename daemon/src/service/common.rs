use crate::service::kaswallet_service::KasWalletService;
use crate::utxo_manager::UtxoManager;
use common::error_location::ErrorLocation;
use common::errors::{RpcError, SyncError, TransactionError, WalletError, WalletResult};
use common::model::WalletSignableTransaction;
use common::status_classify::classify_submit_rpc_error;
use kaspa_consensus_core::sign::Signed::{Fully, Partially};
use kaspa_wallet_core::rpc::RpcApi;
use log::{error, info};
use tokio::sync::MutexGuard;

impl KasWalletService {
    pub(crate) async fn get_virtual_daa_score(&self) -> WalletResult<u64> {
        let block_dag_info =
            self.kaspa_client
                .get_block_dag_info()
                .await
                .map_err(|e| RpcError::Transport {
                    reason: e.to_string(),
                    location: ErrorLocation::capture(),
                })?;

        Ok(block_dag_info.virtual_daa_score)
    }

    pub(crate) async fn check_is_synced(&self) -> WalletResult<()> {
        if !self.sync_manager.is_synced().await {
            // Wallet has not yet completed initial UTXO sync — a transient
            // pre-condition, not a data-integrity issue. Maps to
            // `Code::FailedPrecondition` so clients retry rather than alerting
            // oncall as if it were a server bug.
            Err(WalletError::from(SyncError::NotYetSynced {
                location: ErrorLocation::capture(),
            }))
        } else {
            Ok(())
        }
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
                // Whole-transaction precondition (no per-input index to attribute).
                // Maps to `Code::InvalidArgument` — the caller sent us something
                // they had not finished signing.
                return Err(WalletError::from(TransactionError::NotFullySigned {
                    location: ErrorLocation::capture(),
                }));
            }

            let tx = match &signed_transaction.transaction {
                Fully(tx) => tx,
                Partially(tx) => tx,
            };
            let rpc_transaction = (&tx.tx).into();
            let tx_id = tx.tx.id();
            let input_count = tx.tx.inputs.len();
            let output_count = tx.tx.outputs.len();
            let mass = tx.tx.mass();
            let fee_sompi: u64 = signed_transaction
                .transaction
                .unwrap_ref()
                .entries
                .iter()
                .map(|e| e.as_ref().map(|e| e.amount).unwrap_or(0))
                .sum::<u64>()
                .saturating_sub(tx.tx.outputs.iter().map(|o| o.value).sum::<u64>());

            match self
                .kaspa_client
                .submit_transaction(rpc_transaction, false)
                .await
            {
                Ok(rpc_transaction_id) => {
                    info!(
                        "tx submitted: tx_id={}, mass={}, fee_sompi={}, input_count={}, output_count={}",
                        tx_id, mass, fee_sompi, input_count, output_count
                    );
                    transaction_ids.push(rpc_transaction_id.to_string());

                    utxo_manager
                        .add_mempool_transaction(signed_transaction)
                        .await;
                }
                Err(rpc_err) => {
                    // The kaspa-rpc-core client gives us a typed `RpcError`,
                    // not a `tonic::Status`. Classifying it directly avoids
                    // round-tripping through a fabricated `Status::Internal`
                    // (which would also make the classifier's `InvalidArgument`
                    // branch unreachable) — see PR #27 review on this file.
                    let classified = classify_submit_rpc_error(tx_id, rpc_err);
                    error!(
                        "tx submit failed: tx_id={}, error_kind={}, error_loc={}, input_count={}, output_count={}, mass={}, fee_sompi={}",
                        tx_id,
                        classified.kind_name(),
                        classified.location(),
                        input_count,
                        output_count,
                        mass,
                        fee_sompi
                    );
                    return Err(WalletError::from(classified));
                }
            }
        }

        Ok(transaction_ids)
    }
}
