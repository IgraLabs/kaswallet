use crate::error_location::ErrorLocation;
use crate::errors::rpc::RpcError;
use kaspa_consensus_core::tx::TransactionOutpoint;
use kaspa_hashes::Hash as TransactionId;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TransactionError {
    #[error("{location} BuildFailed: {reason}")]
    BuildFailed {
        reason: String,
        location: ErrorLocation,
    },

    #[error(
        "{location} InsufficientFunds: required={required_sompi} sompi, available={available_sompi} sompi"
    )]
    InsufficientFunds {
        required_sompi: u64,
        available_sompi: u64,
        location: ErrorLocation,
    },

    #[error("{location} UtxoNotFound: {outpoint}")]
    UtxoNotFound {
        outpoint: TransactionOutpoint,
        location: ErrorLocation,
    },

    #[error("{location} SignFailed: input_index={input_index}, reason={reason}")]
    SignFailed {
        input_index: usize,
        reason: String,
        location: ErrorLocation,
    },

    // Whole-transaction signing precondition: inputs were never combined into
    // a fully-signed transaction. Distinct from `SignFailed`, which carries a
    // real `input_index`. Maps to `Code::InvalidArgument`.
    #[error("{location} NotFullySigned")]
    NotFullySigned { location: ErrorLocation },

    // Whole-transaction signature-verification failure on a transaction the
    // daemon itself just signed (`sanity_check_verify`). A failure here is a
    // programmer/state error in the wallet, not caller input — maps to
    // `Code::Internal`.
    #[error("{location} VerifyFailed: {reason}")]
    VerifyFailed {
        reason: String,
        location: ErrorLocation,
    },

    // Signature-verification failure on a wire-supplied payload reaching the
    // broadcast/submit path. The caller sent us a tx that does not verify;
    // map to `Code::InvalidArgument` so oncall is not paged for caller
    // malformed input.
    #[error("{location} SubmitVerifyFailed: {reason}")]
    SubmitVerifyFailed {
        reason: String,
        location: ErrorLocation,
    },

    #[error("{location} InvalidSignature: input_index={input_index}")]
    InvalidSignature {
        input_index: usize,
        location: ErrorLocation,
    },

    #[error("{location} SerializationFailed: stage={stage}, reason={reason}")]
    SerializationFailed {
        stage: &'static str,
        reason: String,
        location: ErrorLocation,
    },

    #[error("{location} MassExceeded: mass={mass}, limit={limit}")]
    MassExceeded {
        mass: u64,
        limit: u64,
        location: ErrorLocation,
    },

    #[error(
        "{location} FeeTooLow: provided={provided_sompi} sompi, required={required_sompi} sompi"
    )]
    FeeTooLow {
        provided_sompi: u64,
        required_sompi: u64,
        location: ErrorLocation,
    },

    #[error("{location} Rejected: tx_id={tx_id}, node_message={node_message}")]
    Rejected {
        tx_id: TransactionId,
        node_message: String,
        location: ErrorLocation,
    },

    #[error("{location} Orphan: tx_id={tx_id}")]
    Orphan {
        tx_id: TransactionId,
        location: ErrorLocation,
    },

    #[error("{location} DoubleSpend: tx_id={tx_id}, conflicting={conflicting_outpoint:?}")]
    DoubleSpend {
        tx_id: TransactionId,
        // Optional because the kaspad gRPC response does not include the
        // conflicting outpoint — the classifier never has it. Storing
        // `Option<_>` is honest about that, instead of fabricating a
        // zero-value outpoint that downstream tooling would treat as real.
        conflicting_outpoint: Option<TransactionOutpoint>,
        location: ErrorLocation,
    },

    #[error("{location} SubmitRpc: tx_id={tx_id}, source=({source})")]
    SubmitRpc {
        tx_id: TransactionId,
        // See SyncError::UtxoFetchFailed for why `#[source]` matters.
        #[source]
        source: Box<RpcError>,
        location: ErrorLocation,
    },
}

impl TransactionError {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::BuildFailed { .. } => "BuildFailed",
            Self::InsufficientFunds { .. } => "InsufficientFunds",
            Self::UtxoNotFound { .. } => "UtxoNotFound",
            Self::SignFailed { .. } => "SignFailed",
            Self::NotFullySigned { .. } => "NotFullySigned",
            Self::VerifyFailed { .. } => "VerifyFailed",
            Self::SubmitVerifyFailed { .. } => "SubmitVerifyFailed",
            Self::InvalidSignature { .. } => "InvalidSignature",
            Self::SerializationFailed { .. } => "SerializationFailed",
            Self::MassExceeded { .. } => "MassExceeded",
            Self::FeeTooLow { .. } => "FeeTooLow",
            Self::Rejected { .. } => "Rejected",
            Self::Orphan { .. } => "Orphan",
            Self::DoubleSpend { .. } => "DoubleSpend",
            Self::SubmitRpc { .. } => "SubmitRpc",
        }
    }

    pub fn location(&self) -> ErrorLocation {
        match self {
            Self::BuildFailed { location, .. }
            | Self::InsufficientFunds { location, .. }
            | Self::UtxoNotFound { location, .. }
            | Self::SignFailed { location, .. }
            | Self::NotFullySigned { location }
            | Self::VerifyFailed { location, .. }
            | Self::SubmitVerifyFailed { location, .. }
            | Self::InvalidSignature { location, .. }
            | Self::SerializationFailed { location, .. }
            | Self::MassExceeded { location, .. }
            | Self::FeeTooLow { location, .. }
            | Self::Rejected { location, .. }
            | Self::Orphan { location, .. }
            | Self::DoubleSpend { location, .. }
            | Self::SubmitRpc { location, .. } => *location,
        }
    }

    pub fn user_message(&self) -> String {
        match self {
            Self::BuildFailed { reason, .. } => format!("transaction build failed: {reason}"),
            Self::InsufficientFunds {
                required_sompi,
                available_sompi,
                ..
            } => format!(
                "insufficient funds: required {required_sompi} sompi, available {available_sompi} sompi"
            ),
            Self::UtxoNotFound { outpoint, .. } => format!("utxo not found: {outpoint}"),
            Self::SignFailed {
                input_index,
                reason,
                ..
            } => format!("signing failed at input {input_index}: {reason}"),
            Self::NotFullySigned { .. } => "transaction is not fully signed".to_string(),
            Self::VerifyFailed { reason, .. } => {
                format!("transaction failed signature verification: {reason}")
            }
            Self::SubmitVerifyFailed { reason, .. } => {
                format!("submitted transaction failed signature verification: {reason}")
            }
            Self::InvalidSignature { input_index, .. } => {
                format!("invalid signature at input {input_index}")
            }
            Self::SerializationFailed { stage, reason, .. } => {
                format!("transaction serialization failed at {stage}: {reason}")
            }
            Self::MassExceeded { mass, limit, .. } => {
                format!("transaction mass {mass} exceeds limit {limit}")
            }
            Self::FeeTooLow {
                provided_sompi,
                required_sompi,
                ..
            } => format!(
                "fee too low: provided {provided_sompi} sompi, required at least {required_sompi} sompi"
            ),
            Self::Rejected {
                tx_id,
                node_message,
                ..
            } => format!("transaction {tx_id} rejected by node: {node_message}"),
            Self::Orphan { tx_id, .. } => format!("transaction {tx_id} is an orphan"),
            Self::DoubleSpend {
                tx_id,
                conflicting_outpoint: Some(outpoint),
                ..
            } => format!("transaction {tx_id} double-spends {outpoint}"),
            Self::DoubleSpend { tx_id, .. } => {
                format!("transaction {tx_id} attempts a double spend")
            }
            Self::SubmitRpc { tx_id, source, .. } => {
                format!(
                    "rpc submit failed for transaction {tx_id}: {}",
                    source.user_message()
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_hashes::Hash;
    use std::error::Error;

    #[test]
    fn insufficient_funds_display() {
        let err = TransactionError::InsufficientFunds {
            required_sompi: 1000,
            available_sompi: 500,
            location: ErrorLocation::capture(),
        };
        let s = err.to_string();
        assert!(s.contains("InsufficientFunds"));
        assert!(s.contains("1000"));
        assert!(s.contains("500"));
        assert_eq!(err.kind_name(), "InsufficientFunds");
    }

    #[test]
    fn submit_rpc_wraps_source() {
        let inner = crate::errors::rpc::RpcError::Transport {
            reason: "closed".into(),
            location: ErrorLocation::capture(),
        };
        let err = TransactionError::SubmitRpc {
            tx_id: Hash::from_bytes([1; 32]),
            source: Box::new(inner),
            location: ErrorLocation::capture(),
        };
        assert!(err.to_string().contains("SubmitRpc"));
        assert_eq!(err.kind_name(), "SubmitRpc");
    }

    #[test]
    fn submit_rpc_exposes_source() {
        let inner = crate::errors::rpc::RpcError::Transport {
            reason: "closed".into(),
            location: ErrorLocation::capture(),
        };
        let err = TransactionError::SubmitRpc {
            tx_id: Hash::from_bytes([1; 32]),
            source: Box::new(inner),
            location: ErrorLocation::capture(),
        };
        assert!(
            err.source().is_some(),
            "source chain must not be terminated"
        );
    }

    #[test]
    fn rejected_carries_node_message() {
        let err = TransactionError::Rejected {
            tx_id: Hash::from_bytes([2; 32]),
            node_message: "insufficient fee".into(),
            location: ErrorLocation::capture(),
        };
        assert!(err.to_string().contains("insufficient fee"));
    }

    #[test]
    fn double_spend_outpoint_is_optional() {
        let _err = TransactionError::DoubleSpend {
            tx_id: Hash::from_bytes([3; 32]),
            conflicting_outpoint: None,
            location: ErrorLocation::capture(),
        };
    }

    #[test]
    fn not_fully_signed_user_message_omits_index() {
        let err = TransactionError::NotFullySigned {
            location: ErrorLocation::capture(),
        };
        assert!(err.user_message().contains("not fully signed"));
        assert!(!err.user_message().contains("input"));
    }
}
