use super::{
    ConfigError, CryptoError, RpcError, StorageError, SyncError, TransactionError, UserInputError,
};
use crate::error_location::ErrorLocation;
use thiserror::Error;
use tonic::{Code, Status};

#[derive(Debug, Clone, Error)]
pub enum WalletError {
    #[error("User input error: {0}")]
    UserInput(Box<UserInputError>),

    #[error("Config error: {0}")]
    Config(Box<ConfigError>),

    #[error("Crypto error: {0}")]
    Crypto(Box<CryptoError>),

    #[error("RPC error: {0}")]
    Rpc(Box<RpcError>),

    #[error("Storage error: {0}")]
    Storage(Box<StorageError>),

    #[error("Transaction error: {0}")]
    Transaction(Box<TransactionError>),

    #[error("Sync error: {0}")]
    Sync(Box<SyncError>),
}

pub type WalletResult<T> = Result<T, WalletError>;

macro_rules! impl_from_sub {
    ($variant:ident, $ty:ty) => {
        impl From<$ty> for WalletError {
            fn from(e: $ty) -> Self {
                WalletError::$variant(Box::new(e))
            }
        }
    };
}

impl_from_sub!(UserInput, UserInputError);
impl_from_sub!(Config, ConfigError);
impl_from_sub!(Crypto, CryptoError);
impl_from_sub!(Rpc, RpcError);
impl_from_sub!(Storage, StorageError);
impl_from_sub!(Transaction, TransactionError);
impl_from_sub!(Sync, SyncError);

impl WalletError {
    pub fn category_name(&self) -> &'static str {
        match self {
            Self::UserInput(_) => "UserInput",
            Self::Config(_) => "Config",
            Self::Crypto(_) => "Crypto",
            Self::Rpc(_) => "Rpc",
            Self::Storage(_) => "Storage",
            Self::Transaction(_) => "Transaction",
            Self::Sync(_) => "Sync",
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

    pub fn location(&self) -> &ErrorLocation {
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

    pub fn to_status(self) -> Status {
        let msg = self.to_string();
        let code = match &self {
            Self::UserInput(_) => Code::InvalidArgument,
            Self::Config(_) => Code::FailedPrecondition,
            Self::Crypto(_) => Code::Internal,
            Self::Rpc(_) => Code::Unavailable,
            Self::Storage(_) => Code::Internal,
            Self::Sync(_) => Code::Internal,
            Self::Transaction(e) => match **e {
                TransactionError::InsufficientFunds { .. }
                | TransactionError::FeeTooLow { .. }
                | TransactionError::InvalidSignature { .. }
                | TransactionError::DoubleSpend { .. } => Code::InvalidArgument,
                TransactionError::Rejected { .. } | TransactionError::Orphan { .. } => {
                    Code::Aborted
                }
                TransactionError::BuildFailed { .. }
                | TransactionError::UtxoNotFound { .. }
                | TransactionError::MassExceeded { .. }
                | TransactionError::SerializationFailed { .. }
                | TransactionError::SignFailed { .. }
                | TransactionError::SubmitRpc { .. } => Code::Internal,
            },
        };
        Status::new(code, msg)
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
            loc: ErrorLocation::capture(),
        }
        .into();
        matches!(e, WalletError::UserInput(_));
    }

    #[test]
    fn kind_name_delegates_to_subenum() {
        let e: WalletError = ConfigError::MissingArgument {
            name: "--rpc-url",
            loc: ErrorLocation::capture(),
        }
        .into();
        assert_eq!(e.kind_name(), "MissingArgument");
    }

    #[test]
    fn category_name_returns_root_label() {
        let e: WalletError = CryptoError::KeyFileNotFound {
            path: "/k".into(),
            loc: ErrorLocation::capture(),
        }
        .into();
        assert_eq!(e.category_name(), "Crypto");
    }

    #[test]
    fn display_includes_category_and_inner() {
        let e: WalletError = TransactionError::InsufficientFunds {
            required_sompi: 100,
            available_sompi: 50,
            loc: ErrorLocation::capture(),
        }
        .into();
        let s = e.to_string();
        assert!(s.starts_with("Transaction error"));
        assert!(s.contains("InsufficientFunds"));
    }
}
