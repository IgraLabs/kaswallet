use super::{
    ConfigError, CryptoError, ErrorCategory, RpcError, StorageError, SyncError, TransactionError,
    UserInputError,
};
use crate::error_location::ErrorLocation;
use thiserror::Error;
use tonic::{Code, Status};

#[derive(Debug, Error)]
pub enum WalletError {
    #[error("User input error: {0}")]
    UserInput(#[from] UserInputError),

    #[error("Config error: {0}")]
    Config(#[from] ConfigError),

    #[error("Crypto error: {0}")]
    Crypto(#[from] CryptoError),

    #[error("RPC error: {0}")]
    Rpc(#[from] RpcError),

    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("Transaction error: {0}")]
    Transaction(#[from] TransactionError),

    #[error("Sync error: {0}")]
    Sync(#[from] SyncError),
}

pub type WalletResult<T> = Result<T, WalletError>;

impl WalletError {
    pub fn category(&self) -> ErrorCategory {
        match self {
            Self::UserInput(_) => ErrorCategory::UserInput,
            Self::Config(_) => ErrorCategory::Config,
            Self::Crypto(_) => ErrorCategory::Crypto,
            Self::Rpc(_) => ErrorCategory::Rpc,
            Self::Storage(_) => ErrorCategory::Storage,
            Self::Transaction(_) => ErrorCategory::Transaction,
            Self::Sync(_) => ErrorCategory::Sync,
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::UserInput(e) => e.kind_name(),
            Self::Config(e) => e.kind_name(),
            Self::Crypto(e) => e.kind_name(),
            Self::Rpc(e) => e.kind_name(),
            Self::Storage(e) => e.kind_name(),
            Self::Transaction(e) => e.kind_name(),
            Self::Sync(e) => e.kind_name(),
        }
    }

    pub fn location(&self) -> ErrorLocation {
        match self {
            Self::UserInput(e) => e.location(),
            Self::Config(e) => e.location(),
            Self::Crypto(e) => e.location(),
            Self::Rpc(e) => e.location(),
            Self::Storage(e) => e.location(),
            Self::Transaction(e) => e.location(),
            Self::Sync(e) => e.location(),
        }
    }

    // Human-readable message safe to ship to remote callers. Excludes
    // `ErrorLocation` (which would leak build-machine paths) and avoids
    // distinguishing variants whose differences are sensitive (see
    // `CryptoError::WrongPassword` / `KeyFileCorrupt`).
    pub fn user_message(&self) -> String {
        match self {
            Self::UserInput(e) => e.user_message(),
            Self::Config(e) => e.user_message(),
            Self::Crypto(e) => e.user_message(),
            Self::Rpc(e) => e.user_message(),
            Self::Storage(e) => e.user_message(),
            Self::Transaction(e) => e.user_message(),
            Self::Sync(e) => e.user_message(),
        }
    }

    pub fn to_status(&self) -> Status {
        let code = match self {
            Self::UserInput(_) => Code::InvalidArgument,
            Self::Config(_) => Code::FailedPrecondition,
            // Both `WrongPassword` and `KeyFileCorrupt` share the same
            // user-facing message (`KEY_DECRYPT_FAILED_MSG`) to avoid an
            // oracle that distinguishes the two. Mapping them to the same
            // gRPC code closes the same oracle at the wire level.
            Self::Crypto(CryptoError::WrongPassword { .. })
            | Self::Crypto(CryptoError::KeyFileCorrupt { .. }) => Code::Unauthenticated,
            Self::Crypto(_) => Code::Internal,
            Self::Rpc(_) => Code::Unavailable,
            Self::Storage(_) => Code::Internal,
            Self::Sync(SyncError::NotYetSynced { .. }) => Code::FailedPrecondition,
            Self::Sync(_) => Code::Internal,
            Self::Transaction(e) => match e {
                TransactionError::InsufficientFunds { .. }
                | TransactionError::FeeTooLow { .. }
                | TransactionError::InvalidSignature { .. }
                | TransactionError::DoubleSpend { .. }
                | TransactionError::NotFullySigned { .. } => Code::InvalidArgument,
                TransactionError::Rejected { .. } | TransactionError::Orphan { .. } => {
                    Code::Aborted
                }
                TransactionError::BuildFailed { .. }
                | TransactionError::UtxoNotFound { .. }
                | TransactionError::MassExceeded { .. }
                | TransactionError::SerializationFailed { .. }
                | TransactionError::SignFailed { .. }
                | TransactionError::VerifyFailed { .. }
                | TransactionError::SubmitRpc { .. } => Code::Internal,
            },
        };
        Status::new(code, self.user_message())
    }
}

impl From<WalletError> for Status {
    fn from(e: WalletError) -> Self {
        e.to_status()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error_location::ErrorLocation;
    use crate::errors::*;

    #[test]
    fn from_subenum_auto_wraps() {
        let e: WalletError = UserInputError::MissingField {
            field: "address",
            location: ErrorLocation::capture(),
        }
        .into();
        assert!(matches!(e, WalletError::UserInput(_)));
    }

    #[test]
    fn kind_name_delegates_to_subenum() {
        let e: WalletError = ConfigError::MissingArgument {
            name: "--rpc-url",
            location: ErrorLocation::capture(),
        }
        .into();
        assert_eq!(e.kind_name(), "MissingArgument");
    }

    #[test]
    fn category_returns_root_label() {
        let e: WalletError = CryptoError::KeyFileNotFound {
            path: "/k".into(),
            location: ErrorLocation::capture(),
        }
        .into();
        assert_eq!(e.category(), ErrorCategory::Crypto);
        assert_eq!(e.category().as_str(), "Crypto");
    }

    #[test]
    fn display_includes_category_and_inner() {
        let e: WalletError = TransactionError::InsufficientFunds {
            required_sompi: 100,
            available_sompi: 50,
            location: ErrorLocation::capture(),
        }
        .into();
        let s = e.to_string();
        assert!(s.starts_with("Transaction error"));
        assert!(s.contains("InsufficientFunds"));
    }

    #[test]
    fn to_status_does_not_leak_location() {
        let e: WalletError = UserInputError::InvalidAmount {
            input: "abc".into(),
            location: ErrorLocation::capture(),
        }
        .into();
        let status = e.to_status();
        assert!(
            !status.message().contains("user_input.rs"),
            "got: {}",
            status.message()
        );
        assert!(status.message().contains("abc"));
    }

    #[test]
    fn wrong_password_and_key_file_corrupt_share_unauthenticated_code() {
        // Both variants share the same user-facing message
        // (KEY_DECRYPT_FAILED_MSG); the gRPC code must match too so a
        // remote observer cannot use `Status::code()` as an oracle to
        // distinguish "wrong password" from "corrupt keys file".
        let wp: WalletError = CryptoError::WrongPassword {
            location: ErrorLocation::capture(),
        }
        .into();
        let kc: WalletError = CryptoError::KeyFileCorrupt {
            reason: "irrelevant".to_string(),
            location: ErrorLocation::capture(),
        }
        .into();
        assert_eq!(wp.to_status().code(), Code::Unauthenticated);
        assert_eq!(kc.to_status().code(), Code::Unauthenticated);
        assert_eq!(wp.to_status().code(), kc.to_status().code());
    }

    #[test]
    fn not_yet_synced_maps_to_failed_precondition() {
        let e: WalletError = SyncError::NotYetSynced {
            location: ErrorLocation::capture(),
        }
        .into();
        assert_eq!(e.to_status().code(), Code::FailedPrecondition);
    }
}
