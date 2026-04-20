use crate::error_location::ErrorLocation;
use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum UserInputError {
    #[error("{loc} InvalidAddress: input={input}, reason={reason}")]
    InvalidAddress {
        input: String,
        reason: String,
        loc: ErrorLocation,
    },

    #[error("{loc} InvalidAmount: input={input}")]
    InvalidAmount { input: String, loc: ErrorLocation },

    #[error("{loc} InvalidTransactionId: input={input}")]
    InvalidTransactionId { input: String, loc: ErrorLocation },

    #[error("{loc} InvalidPrefix: input={input}")]
    InvalidPrefix { input: String, loc: ErrorLocation },

    #[error("{loc} MissingField: {field}")]
    MissingField {
        field: &'static str,
        loc: ErrorLocation,
    },
}

impl UserInputError {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::InvalidAddress { .. } => "InvalidAddress",
            Self::InvalidAmount { .. } => "InvalidAmount",
            Self::InvalidTransactionId { .. } => "InvalidTransactionId",
            Self::InvalidPrefix { .. } => "InvalidPrefix",
            Self::MissingField { .. } => "MissingField",
        }
    }

    pub fn location(&self) -> &ErrorLocation {
        match self {
            Self::InvalidAddress { loc, .. }
            | Self::InvalidAmount { loc, .. }
            | Self::InvalidTransactionId { loc, .. }
            | Self::InvalidPrefix { loc, .. }
            | Self::MissingField { loc, .. } => loc,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_location_and_kind() {
        let err = UserInputError::InvalidAmount {
            input: "abc".into(),
            loc: ErrorLocation::capture(),
        };
        let s = err.to_string();
        assert!(s.contains("InvalidAmount"), "got: {s}");
        assert!(s.contains("abc"), "got: {s}");
        assert!(s.contains("user_input.rs"), "got: {s}");
    }

    #[test]
    fn kind_name_is_stable_key() {
        let err = UserInputError::InvalidAddress {
            input: "bad".into(),
            reason: "malformed".into(),
            loc: ErrorLocation::capture(),
        };
        assert_eq!(err.kind_name(), "InvalidAddress");
    }
}
