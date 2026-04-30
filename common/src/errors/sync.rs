use crate::error_location::ErrorLocation;
use crate::errors::rpc::RpcError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("{location} AddressDerivation: account={account}, index={index}, reason={reason}")]
    AddressDerivation {
        account: u32,
        index: u32,
        reason: String,
        location: ErrorLocation,
    },

    #[error("{location} UtxoFetchFailed: addresses_count={addresses_count}, source=({source})")]
    UtxoFetchFailed {
        addresses_count: usize,
        // `#[source]` makes `Error::source()` return the inner `RpcError`.
        // Without it, anyhow / tracing-error / eyre traversal terminates here
        // and the wrapped RPC failure is invisible to consumers.
        #[source]
        source: Box<RpcError>,
        location: ErrorLocation,
    },

    #[error("{location} UtxoIndexInconsistent: {reason}")]
    UtxoIndexInconsistent {
        reason: String,
        location: ErrorLocation,
    },

    // Wallet has not yet completed initial UTXO sync. Distinct from
    // `UtxoIndexInconsistent`, which describes an actual data-integrity
    // problem. Maps to `Code::FailedPrecondition` so clients retry rather
    // than treating it as a server bug.
    #[error("{location} NotYetSynced")]
    NotYetSynced { location: ErrorLocation },
}

impl SyncError {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::AddressDerivation { .. } => "AddressDerivation",
            Self::UtxoFetchFailed { .. } => "UtxoFetchFailed",
            Self::UtxoIndexInconsistent { .. } => "UtxoIndexInconsistent",
            Self::NotYetSynced { .. } => "NotYetSynced",
        }
    }

    pub fn location(&self) -> ErrorLocation {
        match self {
            Self::AddressDerivation { location, .. }
            | Self::UtxoFetchFailed { location, .. }
            | Self::UtxoIndexInconsistent { location, .. }
            | Self::NotYetSynced { location } => *location,
        }
    }

    pub fn user_message(&self) -> String {
        match self {
            Self::AddressDerivation {
                account,
                index,
                reason,
                ..
            } => format!("address derivation failed (account={account}, index={index}): {reason}"),
            Self::UtxoFetchFailed {
                addresses_count,
                source,
                ..
            } => format!(
                "utxo fetch failed for {addresses_count} addresses: {}",
                source.user_message()
            ),
            Self::UtxoIndexInconsistent { reason, .. } => {
                format!("utxo index inconsistent: {reason}")
            }
            Self::NotYetSynced { .. } => "wallet is not yet synced".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::rpc::RpcError;
    use std::error::Error;

    #[test]
    fn utxo_fetch_failed_wraps_rpc() {
        let inner = RpcError::Transport {
            reason: "closed".into(),
            location: ErrorLocation::capture(),
        };
        let err = SyncError::UtxoFetchFailed {
            addresses_count: 3,
            source: Box::new(inner),
            location: ErrorLocation::capture(),
        };
        assert!(err.to_string().contains("UtxoFetchFailed"));
        assert!(err.to_string().contains("closed"));
        assert_eq!(err.kind_name(), "UtxoFetchFailed");
    }

    #[test]
    fn utxo_fetch_failed_exposes_source() {
        let inner = RpcError::Transport {
            reason: "closed".into(),
            location: ErrorLocation::capture(),
        };
        let err = SyncError::UtxoFetchFailed {
            addresses_count: 3,
            source: Box::new(inner),
            location: ErrorLocation::capture(),
        };
        assert!(
            err.source().is_some(),
            "source chain must not be terminated"
        );
    }
}
