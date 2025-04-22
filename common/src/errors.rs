use log::error;
use std::error::Error;
use thiserror::Error;
use tonic::Status;

#[derive(Debug, Error, Clone)]
pub enum WalletError {
    #[error("{0}")]
    SanityCheckFailed(String),
    #[error("{0}")]
    UserInputError(String),
    #[error("{0}")]
    InternalServerError(String),
}

pub type WalletResult<T> = Result<T, WalletError>;

pub trait WalletResultExt<T> {
    fn to_status(self) -> Result<T, Status>;
}

pub trait ResultExt<T> {
    fn to_wallet_result_internal(self) -> WalletResult<T>;
    fn to_wallet_result_user_input(self) -> WalletResult<T>;
    fn to_wallet_result_sanity_check(self) -> WalletResult<T>;
}

impl<T> WalletResultExt<T> for WalletResult<T> {
    fn to_status(self) -> Result<T, Status> {
        self.map_err(|e| match e {
            WalletError::SanityCheckFailed(msg) => {
                error!("Sanity check failed. {}", msg);
                Status::internal(msg)
            }
            WalletError::UserInputError(msg) => {
                error!("User input error: {}", msg);
                Status::invalid_argument(msg)
            }
            WalletError::InternalServerError(msg) => {
                error!("Internal server error: {}", msg);
                Status::internal(msg)
            }
        })
    }
}

impl<T, E> ResultExt<T> for Result<T, E>
where
    E: Error + Send + Sync,
{
    fn to_wallet_result_internal(self) -> WalletResult<T> {
        self.map_err(|e| WalletError::InternalServerError(e.to_string()))
    }

    fn to_wallet_result_user_input(self) -> WalletResult<T> {
        self.map_err(|e| WalletError::UserInputError(e.to_string()))
    }

    fn to_wallet_result_sanity_check(self) -> WalletResult<T> {
        self.map_err(|e| WalletError::SanityCheckFailed(e.to_string()))
    }
}
