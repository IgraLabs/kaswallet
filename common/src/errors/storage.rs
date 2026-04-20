use crate::error_location::ErrorLocation;
use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum StorageError {
    #[error("{loc} Io: path={path}, reason={reason}")]
    Io {
        path: String,
        reason: String,
        loc: ErrorLocation,
    },

    #[error("{loc} Serialize: kind={kind}, reason={reason}")]
    Serialize {
        kind: &'static str,
        reason: String,
        loc: ErrorLocation,
    },

    #[error("{loc} Deserialize: kind={kind}, reason={reason}")]
    Deserialize {
        kind: &'static str,
        reason: String,
        loc: ErrorLocation,
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

    pub fn location(&self) -> &ErrorLocation {
        match self {
            Self::Io { loc, .. } | Self::Serialize { loc, .. } | Self::Deserialize { loc, .. } => {
                loc
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
            loc: ErrorLocation::capture(),
        };
        assert!(err.to_string().contains("Io"));
        assert!(err.to_string().contains("permission denied"));
        assert_eq!(err.kind_name(), "Io");
    }
}
