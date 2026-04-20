use crate::error_location::ErrorLocation;
use kaspa_addresses::Prefix;
use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum CryptoError {
    #[error("{loc} KeyFileNotFound: {path}")]
    KeyFileNotFound { path: String, loc: ErrorLocation },

    #[error("{loc} KeyFileMalformed: path={path}, reason={reason}")]
    KeyFileMalformed {
        path: String,
        reason: String,
        loc: ErrorLocation,
    },

    #[error("{loc} MnemonicInvalid: {reason}")]
    MnemonicInvalid { reason: String, loc: ErrorLocation },

    #[error("{loc} Bip32Derivation: {reason}")]
    Bip32Derivation { reason: String, loc: ErrorLocation },

    #[error("{loc} SignatureFailed: input_index={input_index}, reason={reason}")]
    SignatureFailed {
        input_index: usize,
        reason: String,
        loc: ErrorLocation,
    },

    #[error("{loc} PrefixMismatch: expected={expected:?}, got={got:?}")]
    PrefixMismatch {
        expected: Prefix,
        got: Prefix,
        loc: ErrorLocation,
    },
}

impl CryptoError {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::KeyFileNotFound { .. } => "KeyFileNotFound",
            Self::KeyFileMalformed { .. } => "KeyFileMalformed",
            Self::MnemonicInvalid { .. } => "MnemonicInvalid",
            Self::Bip32Derivation { .. } => "Bip32Derivation",
            Self::SignatureFailed { .. } => "SignatureFailed",
            Self::PrefixMismatch { .. } => "PrefixMismatch",
        }
    }

    pub fn location(&self) -> &ErrorLocation {
        match self {
            Self::KeyFileNotFound { loc, .. }
            | Self::KeyFileMalformed { loc, .. }
            | Self::MnemonicInvalid { loc, .. }
            | Self::Bip32Derivation { loc, .. }
            | Self::SignatureFailed { loc, .. }
            | Self::PrefixMismatch { loc, .. } => loc,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_mismatch_display() {
        let err = CryptoError::PrefixMismatch {
            expected: Prefix::Mainnet,
            got: Prefix::Testnet,
            loc: ErrorLocation::capture(),
        };
        assert!(err.to_string().contains("PrefixMismatch"));
        assert_eq!(err.kind_name(), "PrefixMismatch");
    }
}
