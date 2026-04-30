use crate::error_location::ErrorLocation;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("{location} Io: path={path}, reason={reason}")]
    Io {
        path: String,
        reason: String,
        location: ErrorLocation,
    },

    #[error("{location} Serialize: kind={kind}, reason={reason}")]
    Serialize {
        kind: &'static str,
        reason: String,
        location: ErrorLocation,
    },

    #[error("{location} Deserialize: kind={kind}, reason={reason}")]
    Deserialize {
        kind: &'static str,
        reason: String,
        location: ErrorLocation,
    },
}

impl StorageError {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Io { .. } => "Io",
            Self::Serialize { .. } => "Serialize",
            Self::Deserialize { .. } => "Deserialize",
        }
    }

    pub fn location(&self) -> ErrorLocation {
        match self {
            Self::Io { location, .. }
            | Self::Serialize { location, .. }
            | Self::Deserialize { location, .. } => *location,
        }
    }

    pub fn user_message(&self) -> String {
        match self {
            Self::Io { path, reason, .. } => format!("i/o error at {path}: {reason}"),
            Self::Serialize { kind, reason, .. } => {
                format!("failed to serialize {kind}: {reason}")
            }
            Self::Deserialize { kind, reason, .. } => {
                format!("failed to deserialize {kind}: {reason}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_display() {
        let err = StorageError::Io {
            path: "/foo".into(),
            reason: "permission denied".into(),
            location: ErrorLocation::capture(),
        };
        assert!(err.to_string().contains("Io"));
        assert!(err.to_string().contains("permission denied"));
        assert_eq!(err.kind_name(), "Io");
    }
}
