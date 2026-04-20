//! Convert tonic::Status responses from kaspad into typed RpcError/TransactionError.
//!
//! Pattern-matches on the Status `code` first, then on `message` substrings for the
//! ambiguous `Internal`/`Aborted` cases. Unrecognised responses fall back to
//! RpcError::KaspadStatus so context is never silently dropped.

use common::error_location::ErrorLocation;
use common::errors::{RpcError, TransactionError};
use kaspa_hashes::Hash as TransactionId;
use tonic::{Code, Status};

#[track_caller]
pub fn classify_submit_status(tx_id: TransactionId, status: Status) -> TransactionError {
    let msg = status.message().to_ascii_lowercase();
    if msg.contains("orphan") {
        return TransactionError::Orphan {
            tx_id,
            loc: ErrorLocation::capture(),
        };
    }
    if msg.contains("double spend") || msg.contains("already spent") {
        use kaspa_consensus_core::tx::TransactionOutpoint;
        return TransactionError::DoubleSpend {
            tx_id,
            conflicting_outpoint: TransactionOutpoint::new(TransactionId::default(), 0),
            loc: ErrorLocation::capture(),
        };
    }
    if status.code() == Code::InvalidArgument || msg.contains("reject") || msg.contains("mempool") {
        return TransactionError::Rejected {
            tx_id,
            node_message: status.message().to_string(),
            loc: ErrorLocation::capture(),
        };
    }
    let rpc = RpcError::KaspadStatus {
        code: status.code(),
        message: status.message().to_string(),
        loc: ErrorLocation::capture(),
    };
    TransactionError::SubmitRpc {
        tx_id,
        source: Box::new(rpc),
        loc: ErrorLocation::capture(),
    }
}

#[track_caller]
pub fn classify_rpc_status(_operation: &'static str, status: Status) -> RpcError {
    RpcError::KaspadStatus {
        code: status.code(),
        message: status.message().to_string(),
        loc: ErrorLocation::capture(),
    }
}

#[track_caller]
pub fn classify_transport(err: tonic::transport::Error) -> RpcError {
    RpcError::Transport {
        reason: err.to_string(),
        loc: ErrorLocation::capture(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphan_message_maps_to_orphan_variant() {
        let err = classify_submit_status(
            TransactionId::default(),
            Status::new(Code::Internal, "transaction is an orphan"),
        );
        assert_eq!(err.kind_name(), "Orphan");
    }

    #[test]
    fn double_spend_message_maps_to_double_spend() {
        let err = classify_submit_status(
            TransactionId::default(),
            Status::new(Code::Internal, "utxo already spent"),
        );
        assert_eq!(err.kind_name(), "DoubleSpend");
    }

    #[test]
    fn rejected_for_invalid_argument() {
        let err = classify_submit_status(
            TransactionId::default(),
            Status::new(Code::InvalidArgument, "bad sig"),
        );
        assert_eq!(err.kind_name(), "Rejected");
    }

    #[test]
    fn fallback_submit_rpc_for_unknown() {
        let err = classify_submit_status(
            TransactionId::default(),
            Status::new(Code::Unknown, "mystery"),
        );
        assert_eq!(err.kind_name(), "SubmitRpc");
    }
}
