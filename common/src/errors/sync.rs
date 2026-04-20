use crate::error_location::ErrorLocation;
use crate::errors::rpc::RpcError;
use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum SyncError {
    #[error("{loc} AddressDerivation: account={account}, index={index}, reason={reason}")]
    AddressDerivation {
        account: u32,
        index: u32,
        reason: String,
        loc: ErrorLocation,
    },

    #[error("{loc} UtxoFetchFailed: addresses_count={addresses_count}, source=({source})")]
    UtxoFetchFailed {
        addresses_count: usize,
        source: Box<RpcError>,
        loc: ErrorLocation,
    },

    #[error("{loc} UtxoIndexInconsistent: {reason}")]
    UtxoIndexInconsistent { reason: String, loc: ErrorLocation },
}

impl SyncError {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::AddressDerivation { .. } => "AddressDerivation",
            Self::UtxoFetchFailed { .. } => "UtxoFetchFailed",
            Self::UtxoIndexInconsistent { .. } => "UtxoIndexInconsistent",
        }
    }

    pub fn location(&self) -> &ErrorLocation {
        match self {
            Self::AddressDerivation { loc, .. }
            | Self::UtxoFetchFailed { loc, .. }
            | Self::UtxoIndexInconsistent { loc, .. } => loc,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::rpc::RpcError;

    #[test]
    fn utxo_fetch_failed_wraps_rpc() {
        let inner = RpcError::Transport {
            reason: "closed".into(),
            loc: ErrorLocation::capture(),
        };
        let err = SyncError::UtxoFetchFailed {
            addresses_count: 3,
            source: Box::new(inner),
            loc: ErrorLocation::capture(),
        };
        assert!(err.to_string().contains("UtxoFetchFailed"));
        assert!(err.to_string().contains("closed"));
        assert_eq!(err.kind_name(), "UtxoFetchFailed");
    }
}
