use crate::error_location::ErrorLocation;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum UserInputError {
    #[error("{location} InvalidAddress: input={input}, reason={reason}")]
    InvalidAddress {
        input: String,
        reason: String,
        location: ErrorLocation,
    },

    #[error("{location} InvalidAmount: input={input}")]
    InvalidAmount {
        input: String,
        location: ErrorLocation,
    },

    #[error("{location} InvalidTransactionId: input={input}")]
    InvalidTransactionId {
        input: String,
        location: ErrorLocation,
    },

    #[error("{location} InvalidPrefix: input={input}")]
    InvalidPrefix {
        input: String,
        location: ErrorLocation,
    },

    #[error("{location} MissingField: {field}")]
    MissingField {
        field: &'static str,
        location: ErrorLocation,
    },

    #[error("{location} InvalidArgument: {reason}")]
    InvalidArgument {
        reason: String,
        location: ErrorLocation,
    },

    #[error("{location} MissingArgument: {name}")]
    MissingArgument {
        name: &'static str,
        location: ErrorLocation,
    },

    #[error("{location} InvalidHex: {reason}")]
    InvalidHex {
        reason: String,
        location: ErrorLocation,
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
            Self::InvalidArgument { .. } => "InvalidArgument",
            Self::MissingArgument { .. } => "MissingArgument",
            Self::InvalidHex { .. } => "InvalidHex",
        }
    }

    pub fn location(&self) -> ErrorLocation {
        match self {
            Self::InvalidAddress { location, .. }
            | Self::InvalidAmount { location, .. }
            | Self::InvalidTransactionId { location, .. }
            | Self::InvalidPrefix { location, .. }
            | Self::MissingField { location, .. }
            | Self::InvalidArgument { location, .. }
            | Self::MissingArgument { location, .. }
            | Self::InvalidHex { location, .. } => *location,
        }
    }

    pub fn user_message(&self) -> String {
        match self {
            Self::InvalidAddress { input, reason, .. } => {
                format!("invalid address {input:?}: {reason}")
            }
            Self::InvalidAmount { input, .. } => format!("invalid amount {input:?}"),
            Self::InvalidTransactionId { input, .. } => {
                format!("invalid transaction id {input:?}")
            }
            Self::InvalidPrefix { input, .. } => format!("invalid prefix {input:?}"),
            Self::MissingField { field, .. } => format!("missing field {field}"),
            Self::InvalidArgument { reason, .. } => reason.clone(),
            Self::MissingArgument { name, .. } => format!("missing argument {name}"),
            Self::InvalidHex { reason, .. } => format!("invalid hex: {reason}"),
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
            location: ErrorLocation::capture(),
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
            location: ErrorLocation::capture(),
        };
        assert_eq!(err.kind_name(), "InvalidAddress");
    }

    #[test]
    fn user_message_omits_location() {
        let err = UserInputError::InvalidAmount {
            input: "abc".into(),
            location: ErrorLocation::capture(),
        };
        let m = err.user_message();
        assert!(!m.contains("user_input.rs"), "got: {m}");
        assert!(m.contains("abc"), "got: {m}");
    }
}
