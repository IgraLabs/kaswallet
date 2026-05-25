use crate::service::kaswallet_service::KasWalletService;
use crate::utxo_manager::UtxoManager;
use common::error_location::ErrorLocation;
use common::errors::{RpcError, SyncError, TransactionError, WalletError, WalletResult};
use common::model::{WalletSignableTransaction, WalletSigned};
use common::status_classify::classify_submit_rpc_error;
use kaspa_wallet_core::rpc::RpcApi;
use tokio::sync::MutexGuard;
use tracing::{error, info};

/// Origin of a `submit_transactions` invocation. Drives stricter validation
/// (re-verify, UTXO ownership check) and mempool-tracker gating for payloads
/// reaching the daemon through the unauthenticated `Broadcast` gRPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubmitSource {
    /// Transactions the daemon itself created and signed (the `Send` RPC).
    /// Trusted: skip re-verify (already verified post-sign) and add to
    /// the local mempool tracker so wallet balance stays consistent.
    Internal,
    /// Transactions arriving from the wire (the `Broadcast` RPC). The
    /// caller is untrusted: re-verify schnorr signatures and validate
    /// that every input outpoint is in this wallet's UTXO set before
    /// forwarding to kaspad. Do NOT update the mempool tracker — those
    /// outputs are caller-supplied and would pollute wallet balance.
    Wire,
}

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
        source: SubmitSource,
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

            if source == SubmitSource::Wire {
                // Lane gate: reject any wire-supplied tx whose subnetwork
                // id does not match the daemon's configured lane. Without
                // this, a daemon configured for IGRA `97b10000…` could be
                // coerced into broadcasting native (or other-lane) txs
                // under its network identity.
                self.ensure_subnetwork_id_matches(&tx.tx.subnetwork_id)?;
                // Defensive validation BEFORE upstream's `as_verifiable()`
                // which asserts (panics) on entries.len != inputs.len or
                // any None entry. The proto layer also enforces this in
                // `signable_transaction_from_proto`, but we defend in depth
                // since panicking the daemon on attacker input is exactly
                // the failure mode this code path tries to prevent.
                if tx.entries.len() != tx.tx.inputs.len() {
                    return Err(WalletError::from(TransactionError::SubmitVerifyFailed {
                        reason: format!(
                            "entries length {} does not match inputs length {}",
                            tx.entries.len(),
                            tx.tx.inputs.len()
                        ),
                        location: ErrorLocation::capture(),
                    }));
                }
                if tx.entries.iter().any(|e| e.is_none()) {
                    return Err(WalletError::from(TransactionError::SubmitVerifyFailed {
                        reason: "entries must not contain None for verification".to_string(),
                        location: ErrorLocation::capture(),
                    }));
                }
                // Ownership check: every input must reference a UTXO this
                // wallet currently owns. Without this, the verify call
                // only proves "signatures match entries[i].script_public_key" —
                // an attacker can craft a self-consistent payload spending
                // their own UTXOs and have the daemon broadcast it under
                // the wallet's network identity (relay attack).
                let utxos_by_outpoint = utxo_manager.utxos_by_outpoint();
                for input in &tx.tx.inputs {
                    let outpoint = input.previous_outpoint.into();
                    if !utxos_by_outpoint.contains_key(&outpoint) {
                        return Err(WalletError::from(TransactionError::UtxoNotFound {
                            outpoint: input.previous_outpoint,
                            location: ErrorLocation::capture(),
                        }));
                    }
                }
                // Re-verify schnorr signatures. Only proves the signatures
                // match the wire-supplied entries — UTXO ownership was
                // already established above.
                kaspa_consensus_core::sign::verify(&tx.as_verifiable()).map_err(|e| {
                    WalletError::from(TransactionError::SubmitVerifyFailed {
                        reason: e.to_string(),
                        location: ErrorLocation::capture(),
                    })
                })?;
            }
            let rpc_transaction = (&tx.tx).into();
            let tx_id = tx.tx.id();
            let input_count = tx.tx.inputs.len();
            let output_count = tx.tx.outputs.len();
            let mass = tx.tx.mass();
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
                        source = ?source,
                        "tx submitted"
                    );
                    transaction_ids.push(rpc_transaction_id.to_string());

                    // Only update the wallet's mempool tracker for
                    // internally-built transactions. Wire-supplied
                    // outputs are caller-controlled — adding them would
                    // let any gRPC caller plant fake "received" entries
                    // into the wallet's balance view.
                    if source == SubmitSource::Internal {
                        utxo_manager
                            .add_mempool_transaction(signed_transaction)
                            .await;
                    }
                }
                Err(rpc_err) => {
                    // The kaspa-rpc-core client gives us a typed `RpcError`,
                    // not a `tonic::Status`. Classifying it directly avoids
                    // round-tripping through a fabricated `Status::Internal`
                    // (which would also make the classifier's `InvalidArgument`
                    // branch unreachable) — see PR #27 review on this file.
                    let classified = classify_submit_rpc_error(tx_id, rpc_err);
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
