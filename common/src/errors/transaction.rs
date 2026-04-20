use crate::error_location::ErrorLocation;
use crate::errors::rpc::RpcError;
use kaspa_consensus_core::tx::TransactionOutpoint;
use kaspa_hashes::Hash as TransactionId;
use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum TransactionError {
    #[error("{loc} BuildFailed: {reason}")]
    BuildFailed { reason: String, loc: ErrorLocation },

    #[error(
        "{loc} InsufficientFunds: required={required_sompi} sompi, available={available_sompi} sompi"
    )]
    InsufficientFunds {
        required_sompi: u64,
        available_sompi: u64,
        loc: ErrorLocation,
    },

    #[error("{loc} UtxoNotFound: {outpoint}")]
    UtxoNotFound {
        outpoint: TransactionOutpoint,
        loc: ErrorLocation,
    },

    #[error("{loc} SignFailed: input_index={input_index}, reason={reason}")]
    SignFailed {
        input_index: usize,
        reason: String,
        loc: ErrorLocation,
    },

    #[error("{loc} InvalidSignature: input_index={input_index}")]
    InvalidSignature {
        input_index: usize,
        loc: ErrorLocation,
    },

    #[error("{loc} SerializationFailed: stage={stage}, reason={reason}")]
    SerializationFailed {
        stage: &'static str,
        reason: String,
        loc: ErrorLocation,
    },

    #[error("{loc} MassExceeded: mass={mass}, limit={limit}")]
    MassExceeded {
        mass: u64,
        limit: u64,
        loc: ErrorLocation,
    },

    #[error("{loc} FeeTooLow: provided={provided_sompi} sompi, required={required_sompi} sompi")]
    FeeTooLow {
        provided_sompi: u64,
        required_sompi: u64,
        loc: ErrorLocation,
    },

    #[error("{loc} Rejected: tx_id={tx_id}, node_message={node_message}")]
    Rejected {
        tx_id: TransactionId,
        node_message: String,
        loc: ErrorLocation,
    },

    #[error("{loc} Orphan: tx_id={tx_id}")]
    Orphan {
        tx_id: TransactionId,
        loc: ErrorLocation,
    },

    #[error("{loc} DoubleSpend: tx_id={tx_id}, conflicting={conflicting_outpoint}")]
    DoubleSpend {
        tx_id: TransactionId,
        conflicting_outpoint: TransactionOutpoint,
        loc: ErrorLocation,
    },

    #[error("{loc} SubmitRpc: tx_id={tx_id}, source=({source})")]
    SubmitRpc {
        tx_id: TransactionId,
        source: Box<RpcError>,
        loc: ErrorLocation,
    },
}

impl TransactionError {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::BuildFailed { .. } => "BuildFailed",
            Self::InsufficientFunds { .. } => "InsufficientFunds",
            Self::UtxoNotFound { .. } => "UtxoNotFound",
            Self::SignFailed { .. } => "SignFailed",
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

    pub fn location(&self) -> &ErrorLocation {
        match self {
            Self::BuildFailed { loc, .. }
            | Self::InsufficientFunds { loc, .. }
            | Self::UtxoNotFound { loc, .. }
            | Self::SignFailed { loc, .. }
            | Self::InvalidSignature { loc, .. }
            | Self::SerializationFailed { loc, .. }
            | Self::MassExceeded { loc, .. }
            | Self::FeeTooLow { loc, .. }
            | Self::Rejected { loc, .. }
            | Self::Orphan { loc, .. }
            | Self::DoubleSpend { loc, .. }
            | Self::SubmitRpc { loc, .. } => loc,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_consensus_core::tx::TransactionOutpoint;
    use kaspa_hashes::Hash;

    #[test]
    fn insufficient_funds_display() {
        let err = TransactionError::InsufficientFunds {
            required_sompi: 1000,
            available_sompi: 500,
            loc: ErrorLocation::capture(),
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
            loc: ErrorLocation::capture(),
        };
        let err = TransactionError::SubmitRpc {
            tx_id: Hash::from_bytes([1; 32]),
            source: Box::new(inner),
            loc: ErrorLocation::capture(),
        };
        assert!(err.to_string().contains("SubmitRpc"));
        assert_eq!(err.kind_name(), "SubmitRpc");
    }

    #[test]
    fn rejected_carries_node_message() {
        let err = TransactionError::Rejected {
            tx_id: Hash::from_bytes([2; 32]),
            node_message: "insufficient fee".into(),
            loc: ErrorLocation::capture(),
        };
        assert!(err.to_string().contains("insufficient fee"));
    }

    #[test]
    fn double_spend_carries_outpoint() {
        let _err = TransactionError::DoubleSpend {
            tx_id: Hash::from_bytes([3; 32]),
            conflicting_outpoint: TransactionOutpoint::new(Hash::from_bytes([4; 32]), 7),
            loc: ErrorLocation::capture(),
        };
    }
}
