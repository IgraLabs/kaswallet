use crate::error_location::ErrorLocation;
use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum ConfigError {
    #[error("{loc} InvalidPath: path={path}, reason={reason}")]
    InvalidPath {
        path: String,
        reason: String,
        loc: ErrorLocation,
    },

    #[error("{loc} InvalidLogLevel: value={value}")]
    InvalidLogLevel { value: String, loc: ErrorLocation },

    #[error("{loc} MissingArgument: {name}")]
    MissingArgument {
        name: &'static str,
        loc: ErrorLocation,
    },
}

impl ConfigError {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::InvalidPath { .. } => "InvalidPath",
            Self::InvalidLogLevel { .. } => "InvalidLogLevel",
            Self::MissingArgument { .. } => "MissingArgument",
        }
    }

    pub fn location(&self) -> &ErrorLocation {
        match self {
            Self::InvalidPath { loc, .. }
            | Self::InvalidLogLevel { loc, .. }
            | Self::MissingArgument { loc, .. } => loc,
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
            loc: ErrorLocation::capture(),
        };
        assert!(err.to_string().contains("InvalidPath"));
        assert_eq!(err.kind_name(), "InvalidPath");
    }
}
