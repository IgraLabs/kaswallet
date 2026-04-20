use crate::error_location::ErrorLocation;
use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum RpcError {
    #[error("{loc} Connect: endpoint={endpoint}, reason={reason}")]
    Connect {
        endpoint: String,
        reason: String,
        loc: ErrorLocation,
    },

    #[error("{loc} Transport: {reason}")]
    Transport { reason: String, loc: ErrorLocation },

    #[error("{loc} KaspadStatus: code={code:?}, message={message}")]
    KaspadStatus {
        code: tonic::Code,
        message: String,
        loc: ErrorLocation,
    },

    #[error("{loc} Timeout: operation={operation}, elapsed_ms={elapsed_ms}")]
    Timeout {
        operation: &'static str,
        elapsed_ms: u64,
        loc: ErrorLocation,
    },

    #[error("{loc} MalformedResponse: operation={operation}, reason={reason}")]
    MalformedResponse {
        operation: &'static str,
        reason: String,
        loc: ErrorLocation,
    },
}

impl RpcError {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Connect { .. } => "Connect",
            Self::Transport { .. } => "Transport",
            Self::KaspadStatus { .. } => "KaspadStatus",
            Self::Timeout { .. } => "Timeout",
            Self::MalformedResponse { .. } => "MalformedResponse",
        }
    }

    pub fn location(&self) -> &ErrorLocation {
        match self {
            Self::Connect { loc, .. }
            | Self::Transport { loc, .. }
            | Self::KaspadStatus { loc, .. }
            | Self::Timeout { loc, .. }
            | Self::MalformedResponse { loc, .. } => loc,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kaspad_status_display() {
        let err = RpcError::KaspadStatus {
            code: tonic::Code::Aborted,
            message: "mempool rejected".into(),
            loc: ErrorLocation::capture(),
        };
        assert!(err.to_string().contains("KaspadStatus"));
        assert!(err.to_string().contains("mempool rejected"));
        assert_eq!(err.kind_name(), "KaspadStatus");
    }
}
