use crate::service::kaswallet_service::KasWalletService;
use crate::utxo_manager::UtxoManager;
use common::error_location::ErrorLocation;
use common::errors::{RpcError, SyncError, TransactionError, WalletError, WalletResult};
use common::model::WalletSignableTransaction;
use common::model::WalletSigned;
use common::status_classify::classify_submit_rpc_error;
use kaspa_wallet_core::rpc::RpcApi;
use tokio::sync::MutexGuard;
use tracing::{error, info, warn};

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
        // Bind the guard so it lives for the body, not the statement —
        // `let _ = ...` would drop the MutexGuard immediately and remove
        // the intended serialization across concurrent broadcast/send.
        let _guard = self.submit_transaction_mutex.lock().await;

        let mut transaction_ids = vec![];
        for signed_transaction in signed_transactions {
            // Encode the "must be Fully signed" precondition on the match
            // itself so the type system enforces it. A future reorder
            // cannot accidentally submit a Partially-signed payload.
            let tx = match &signed_transaction.transaction {
                WalletSigned::Fully(tx) => tx,
                WalletSigned::Partially(_) => {
                    return Err(WalletError::from(TransactionError::NotFullySigned {
                        location: ErrorLocation::capture(),
                    }));
                }
            };

            // Lane gate is unconditional: a lane-bound daemon must never
            // sign or broadcast a cross-lane tx, no matter the source.
            // The check is a no-op for Send (the tx generator built with
            // the configured lane) and catches misrouted wire payloads.
            self.ensure_subnetwork_id_matches(&tx.tx.subnetwork_id)?;

            let rpc_transaction = (&tx.tx).into();
            let tx_id = tx.tx.id();
            let input_count = tx.tx.inputs.len();
            let output_count = tx.tx.outputs.len();
            let mass = tx.tx.storage_mass();
            let fee_sompi: u64 = tx
                .entries
                .iter()
                .map(|e| e.as_ref().map(|e| e.amount).unwrap_or(0))
                .sum::<u64>()
                .saturating_sub(tx.tx.outputs.iter().map(|o| o.value).sum::<u64>());
            // Capture lane / consensus-version on the tx itself (not from
            // the daemon's configured lane) so the log line truthfully
            // describes what was sent to kaspad even if the two diverge.
            // `SubnetworkId: Copy` so this is a cheap byte-array copy.
            let subnetwork_id = tx.tx.subnetwork_id;
            let tx_version = tx.tx.version;

            match self
                .kaspa_client
                .submit_transaction(rpc_transaction, false)
                .await
            {
                Ok(rpc_transaction_id) => {
                    info!(
                        tx_id = %tx_id,
                        subnetwork_id = %subnetwork_id,
                        tx_version,
                        mass,
                        fee_sompi,
                        input_count,
                        output_count,
                        "tx submitted"
                    );
                    transaction_ids.push(rpc_transaction_id.to_string());

                    // Mempool-track every successful submit, regardless of
                    // RPC source. The daemon is deployed on internal
                    // networks where the gRPC trust boundary lives at the
                    // perimeter, not at the RPC endpoint — there is no
                    // attacker model that justifies skipping the input-
                    // removal / output-addition that keeps the wallet's
                    // view consistent across rapid back-to-back submits.
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
                    // On Orphan, dump the wallet's view of each input
                    // outpoint so we can distinguish (a) confirmed-then-
                    // reorged (block_daa_score > 0, is_unconfirmed=false),
                    // (b) NOT_IN_WALLET_VIEW (wallet desynced post-build),
                    // and (c) is_unconfirmed=true (filter saw it but the
                    // selector still picked it — a bug we want to find).
                    if matches!(classified, TransactionError::Orphan { .. }) {
                        let view = utxo_manager.utxos_by_outpoint();
                        let parents: Vec<String> = tx
                            .tx
                            .inputs
                            .iter()
                            .map(|input| {
                                let op = input.previous_outpoint;
                                match view.get(&op.into()) {
                                    Some(u) => format!(
                                        "{op}(block_daa_score={}, is_unconfirmed={}, is_coinbase={})",
                                        u.utxo_entry.block_daa_score,
                                        u.utxo_entry.is_unconfirmed,
                                        u.utxo_entry.is_coinbase,
                                    ),
                                    None => format!("{op}(NOT_IN_WALLET_VIEW)"),
                                }
                            })
                            .collect();
                        warn!(
                            tx_id = %tx_id,
                            subnetwork_id = %subnetwork_id,
                            tx_version,
                            parents = ?parents,
                            "orphan parents — wallet's view of each input outpoint at submit time"
                        );
                    }
                    error!(
                        tx_id = %tx_id,
                        subnetwork_id = %subnetwork_id,
                        tx_version,
                        error_kind = classified.kind_name(),
                        error_loc = %classified.location(),
                        input_count,
                        output_count,
                        mass,
                        fee_sompi,
                        "tx submit failed"
                    );
                    return Err(WalletError::from(classified));
                }
            }
        }

        Ok(transaction_ids)
    }
}
