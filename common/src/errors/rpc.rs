use crate::error_location::ErrorLocation;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("{location} Connect: endpoint={endpoint}, reason={reason}")]
    Connect {
        endpoint: String,
        reason: String,
        location: ErrorLocation,
    },

    #[error("{location} Transport: {reason}")]
    Transport {
        reason: String,
        location: ErrorLocation,
    },

    // `operation` identifies the RPC method that produced the status. We carry
    // it as `&'static str` because every call site is a constant string literal
    // (the gRPC method name) and it lets us avoid allocating one `String` per
    // RPC failure. Display includes it so log lines tell ops which call broke
    // without having to inspect `ErrorLocation`.
    #[error("{location} KaspadStatus: operation={operation}, code={code:?}, message={message}")]
    KaspadStatus {
        operation: &'static str,
        code: tonic::Code,
        message: String,
        location: ErrorLocation,
    },

    #[error("{location} Timeout: operation={operation}, elapsed_ms={elapsed_ms}")]
    Timeout {
        operation: &'static str,
        elapsed_ms: u64,
        location: ErrorLocation,
    },

    #[error("{location} MalformedResponse: operation={operation}, reason={reason}")]
    MalformedResponse {
        operation: &'static str,
        reason: String,
        location: ErrorLocation,
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

    pub fn location(&self) -> ErrorLocation {
        match self {
            Self::Connect { location, .. }
            | Self::Transport { location, .. }
            | Self::KaspadStatus { location, .. }
            | Self::Timeout { location, .. }
            | Self::MalformedResponse { location, .. } => *location,
        }
    }

    pub fn user_message(&self) -> String {
        match self {
            Self::Connect {
                endpoint, reason, ..
            } => format!("could not connect to {endpoint}: {reason}"),
            Self::Transport { reason, .. } => format!("transport error: {reason}"),
            Self::KaspadStatus {
                operation,
                code,
                message,
                ..
            } => format!("kaspad rpc {operation} failed [{code:?}]: {message}"),
            Self::Timeout {
                operation,
                elapsed_ms,
                ..
            } => format!("rpc {operation} timed out after {elapsed_ms} ms"),
            Self::MalformedResponse {
                operation, reason, ..
            } => format!("malformed response from {operation}: {reason}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kaspad_status_display() {
        let err = RpcError::KaspadStatus {
            operation: "submit",
            code: tonic::Code::Aborted,
            message: "mempool rejected".into(),
            location: ErrorLocation::capture(),
        };
        assert!(err.to_string().contains("KaspadStatus"));
        assert!(err.to_string().contains("mempool rejected"));
        assert!(err.to_string().contains("submit"));
        assert_eq!(err.kind_name(), "KaspadStatus");
    }

    #[test]
    fn user_message_excludes_location() {
        let err = RpcError::KaspadStatus {
            operation: "get_balance",
            code: tonic::Code::Internal,
            message: "boom".into(),
            location: ErrorLocation::capture(),
        };
        assert!(!err.user_message().contains("rpc.rs"));
        assert!(err.user_message().contains("get_balance"));
    }
}
