use thiserror::Error;
#[derive(Debug, Error, Clone)]
pub enum WalletError {
    #[error("{0}")]
    SanityCheckFailed(String),
    #[error("{0}")]
    UserInputError(String),
}
