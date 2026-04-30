//! Classify external RPC failures into typed `WalletError` sub-enums.
//!
//! # What this module decides
//!
//! When kaspad answers an RPC over gRPC, the response is a `tonic::Status`
//! consisting of:
//!   * `code`  — a `tonic::Code` enum (e.g. `InvalidArgument`, `Unavailable`).
//!   * `message` — free-form text from the kaspad node.
//!
//! Different kinds of failure want different `TransactionError` variants
//! (`Orphan`, `DoubleSpend`, `Rejected`, …) plus a fallback to `RpcError`.
//! This module is the single place where that mapping lives.
//!
//! # Why classification is principled, not greedy substring matching
//!
//! kaspad's `message` text is **not a stable contract**. A future release can
//! reword "transaction is an orphan" to "tx orphan: missing parent" without
//! warning. Greedy substring matching would silently reclassify errors and
//! flip gRPC codes in callers' faces.
//!
//! We mitigate this two ways:
//!   1. `status.code()` is checked first — it is part of the gRPC contract
//!      and rarely lies.
//!   2. Substring patterns are deliberately narrow (whole phrases, not single
//!      ambiguous words). For example, we match `"already spent"` rather
//!      than the bare word `"mempool"` — the latter would misfire on benign
//!      messages like `"added to mempool"`.
//!
//! # Sanitisation of `node_message`
//!
//! kaspad messages reach our terminal output and log files. A malicious node
//! could embed control characters or ANSI escape sequences. We strip those
//! and bound the length before storing the message.
//!
//! # Decision table (classify_submit_status)
//!
//! ```text
//! status.code() | message contains              | result
//! --------------|-------------------------------|----------------------------
//! any           | "is an orphan" / "tx orphan"  | TransactionError::Orphan
//! any           | "already spent"               | TransactionError::DoubleSpend (no outpoint — node doesn't tell us which one)
//! any           | "rejected by mempool"         | TransactionError::Rejected
//! InvalidArg    | (anything)                    | TransactionError::Rejected
//! Aborted       | (anything)                    | TransactionError::Rejected
//! other         | (anything)                    | TransactionError::SubmitRpc { source: RpcError::KaspadStatus }
//! ```

use crate::error_location::ErrorLocation;
use crate::errors::{RpcError, TransactionError};
use kaspa_hashes::Hash as TransactionId;
use tonic::{Code, Status};

/// Maximum length of `node_message` we will preserve. Anything longer is
/// truncated with an ellipsis. Picked to fit a typical journal/log line.
const NODE_MESSAGE_MAX_LEN: usize = 512;

/// Strip control characters (incl. ANSI escapes) and bound the length of a
/// node-supplied message so it is safe to put into terminals and log files.
fn sanitize_node_message(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control() || *c == ' ')
        .collect();
    if cleaned.len() > NODE_MESSAGE_MAX_LEN {
        let mut truncated: String = cleaned.chars().take(NODE_MESSAGE_MAX_LEN).collect();
        truncated.push('…');
        truncated
    } else {
        cleaned
    }
}

#[track_caller]
pub fn classify_submit_status(tx_id: TransactionId, status: Status) -> TransactionError {
    let raw_msg = status.message();
    let msg = raw_msg.to_ascii_lowercase();
    let sanitized = sanitize_node_message(raw_msg);
    let code = status.code();

    // Orphan: kaspad messaging consistently says "is an orphan" or "tx orphan".
    // Bare "orphan" matches both forms without false positives.
    if msg.contains("is an orphan") || msg.contains("tx orphan") {
        return TransactionError::Orphan {
            tx_id,
            location: ErrorLocation::capture(),
        };
    }

    // Double-spend: "already spent" is the canonical kaspad phrasing. We do
    // *not* fabricate the conflicting outpoint — the gRPC response does not
    // contain it, and inventing a zero outpoint would mislead log dashboards.
    if msg.contains("already spent") || msg.contains("double-spend") {
        return TransactionError::DoubleSpend {
            tx_id,
            conflicting_outpoint: None,
            location: ErrorLocation::capture(),
        };
    }

    // Rejected: prefer to drive off the gRPC code (`InvalidArgument`,
    // `Aborted`); fall back to a *narrow* substring (`"rejected by mempool"`)
    // rather than the bare word `"mempool"` — that would misfire on benign
    // messages like `"transaction added to mempool"`.
    let rejected_by_substring = msg.contains("rejected by mempool")
        || msg.contains("transaction rejected")
        || msg.contains("mempool rejected");
    if code == Code::InvalidArgument || code == Code::Aborted || rejected_by_substring {
        return TransactionError::Rejected {
            tx_id,
            node_message: sanitized,
            location: ErrorLocation::capture(),
        };
    }

    // Fallback: preserve the original tonic context inside an `RpcError`.
    let rpc = RpcError::KaspadStatus {
        operation: "submit_transaction",
        code,
        message: sanitized,
        location: ErrorLocation::capture(),
    };
    TransactionError::SubmitRpc {
        tx_id,
        source: Box::new(rpc),
        location: ErrorLocation::capture(),
    }
}

/// Classify a generic kaspad gRPC failure (used for non-submit RPCs like
/// `get_balance`, `get_addresses`, etc).
///
/// `operation` identifies the RPC method that produced the status. We carry
/// it as `&'static str` so every caller passes a constant string literal —
/// no per-error allocations, and operators reading logs can tell which call
/// broke without inspecting `ErrorLocation`.
#[track_caller]
pub fn classify_rpc_status(operation: &'static str, status: Status) -> RpcError {
    RpcError::KaspadStatus {
        operation,
        code: status.code(),
        message: sanitize_node_message(status.message()),
        location: ErrorLocation::capture(),
    }
}

/// Classify a tonic transport-level error. Caller passes the endpoint string
/// it was trying to talk to so we never lose connection context.
#[track_caller]
pub fn classify_transport(endpoint: &str, err: tonic::transport::Error) -> RpcError {
    RpcError::Connect {
        endpoint: endpoint.to_string(),
        reason: err.to_string(),
        location: ErrorLocation::capture(),
    }
}

/// Classify a `kaspa_rpc_core::RpcError` returned by the in-process RPC client
/// (no tonic round-trip involved). This is used by the daemon when it talks
/// to its embedded kaspad client; we cannot fabricate a `tonic::Status` for
/// these — see PR #27 review comment on `daemon/src/service/common.rs:94`.
///
/// Callers that need transaction-submit semantics (Rejected/Orphan/DoubleSpend
/// classification on `submit_transaction` failures) should use
/// `classify_submit_rpc_error` instead.
#[track_caller]
pub fn classify_kaspad_rpc_error(
    operation: &'static str,
    err: kaspa_rpc_core::RpcError,
) -> RpcError {
    RpcError::KaspadStatus {
        operation,
        code: Code::Unknown,
        message: sanitize_node_message(&err.to_string()),
        location: ErrorLocation::capture(),
    }
}

/// Classify a `submit_transaction` failure that came from the in-process
/// kaspa-rpc-core client (so we have a typed `RpcError`, not a `Status`). We
/// pattern-match the message against the same orphan/double-spend/rejection
/// vocabulary as `classify_submit_status`.
#[track_caller]
pub fn classify_submit_rpc_error(
    tx_id: TransactionId,
    err: kaspa_rpc_core::RpcError,
) -> TransactionError {
    let raw = err.to_string();
    let msg = raw.to_ascii_lowercase();
    let sanitized = sanitize_node_message(&raw);

    if msg.contains("is an orphan") || msg.contains("tx orphan") {
        return TransactionError::Orphan {
            tx_id,
            location: ErrorLocation::capture(),
        };
    }
    if msg.contains("already spent") || msg.contains("double-spend") {
        return TransactionError::DoubleSpend {
            tx_id,
            conflicting_outpoint: None,
            location: ErrorLocation::capture(),
        };
    }
    if msg.contains("rejected by mempool")
        || msg.contains("transaction rejected")
        || msg.contains("mempool rejected")
    {
        return TransactionError::Rejected {
            tx_id,
            node_message: sanitized,
            location: ErrorLocation::capture(),
        };
    }

    let rpc = RpcError::KaspadStatus {
        operation: "submit_transaction",
        code: Code::Unknown,
        message: sanitized,
        location: ErrorLocation::capture(),
    };
    TransactionError::SubmitRpc {
        tx_id,
        source: Box::new(rpc),
        location: ErrorLocation::capture(),
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
    fn double_spend_message_maps_to_double_spend_with_no_outpoint() {
        let err = classify_submit_status(
            TransactionId::default(),
            Status::new(Code::Internal, "utxo already spent"),
        );
        assert_eq!(err.kind_name(), "DoubleSpend");
        match err {
            TransactionError::DoubleSpend {
                conflicting_outpoint,
                ..
            } => {
                assert!(
                    conflicting_outpoint.is_none(),
                    "must not fabricate outpoint"
                );
            }
            _ => panic!("wrong variant"),
        }
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

    #[test]
    fn benign_added_to_mempool_does_not_classify_as_rejected() {
        // Regression test for the original "mempool" substring trap. A future
        // kaspad message like "transaction added to mempool" must NOT be
        // misclassified — it is not even an error in the first place, but if
        // such a string ever did flow through here, it should fall through to
        // the SubmitRpc fallback rather than masquerade as Rejected.
        let err = classify_submit_status(
            TransactionId::default(),
            Status::new(Code::Internal, "transaction added to mempool"),
        );
        assert_eq!(
            err.kind_name(),
            "SubmitRpc",
            "benign mempool string must not become Rejected"
        );
    }

    #[test]
    fn sanitises_control_characters_in_node_message() {
        // ANSI escape + bell + tab — none should reach logs verbatim.
        let raw = "rejected\x1b[31m by\x07 mempool: \tbroken";
        let err = classify_submit_status(TransactionId::default(), Status::new(Code::Aborted, raw));
        match err {
            TransactionError::Rejected { node_message, .. } => {
                assert!(!node_message.contains('\x1b'));
                assert!(!node_message.contains('\x07'));
                assert!(!node_message.contains('\t'));
            }
            _ => panic!("expected Rejected, got {err}"),
        }
    }

    #[test]
    fn sanitises_caps_node_message_length() {
        let huge = "a".repeat(NODE_MESSAGE_MAX_LEN * 2);
        let err =
            classify_submit_status(TransactionId::default(), Status::new(Code::Aborted, huge));
        match err {
            TransactionError::Rejected { node_message, .. } => {
                assert!(node_message.chars().count() <= NODE_MESSAGE_MAX_LEN + 1);
            }
            _ => panic!("expected Rejected"),
        }
    }
}
