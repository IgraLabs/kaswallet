use crate::error_location::ErrorLocation;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{location} InvalidPath: path={path}, reason={reason}")]
    InvalidPath {
        path: String,
        reason: String,
        location: ErrorLocation,
    },

    #[error("{location} InvalidLogLevel: value={value}")]
    InvalidLogLevel {
        value: String,
        location: ErrorLocation,
    },

    #[error("{location} SubscriberAlreadyInitialized: reason={reason}")]
    SubscriberAlreadyInitialized {
        reason: String,
        location: ErrorLocation,
    },

    #[error("{location} MissingArgument: {name}")]
    MissingArgument {
        name: &'static str,
        location: ErrorLocation,
    },
}

impl ConfigError {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::InvalidPath { .. } => "InvalidPath",
            Self::InvalidLogLevel { .. } => "InvalidLogLevel",
            Self::SubscriberAlreadyInitialized { .. } => "SubscriberAlreadyInitialized",
            Self::MissingArgument { .. } => "MissingArgument",
        }
    }

    pub fn location(&self) -> ErrorLocation {
        match self {
            Self::InvalidPath { location, .. }
            | Self::InvalidLogLevel { location, .. }
            | Self::SubscriberAlreadyInitialized { location, .. }
            | Self::MissingArgument { location, .. } => *location,
        }
    }

    pub fn user_message(&self) -> String {
        match self {
            Self::InvalidPath { path, reason, .. } => {
                format!("invalid config path {path:?}: {reason}")
            }
            Self::InvalidLogLevel { value, .. } => format!("invalid log level {value:?}"),
            Self::SubscriberAlreadyInitialized { reason, .. } => {
                format!("tracing subscriber already initialized: {reason}")
            }
            Self::MissingArgument { name, .. } => format!("missing argument {name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_kind_name() {
        let err = ConfigError::InvalidPath {
            path: "/bad".into(),
            reason: "nope".into(),
            location: ErrorLocation::capture(),
        };
        assert!(err.to_string().contains("InvalidPath"));
        assert_eq!(err.kind_name(), "InvalidPath");
    }
}
