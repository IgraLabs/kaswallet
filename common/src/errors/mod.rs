//! Typed error tree for kaswallet. See
//! `docs/superpowers/specs/2026-04-14-wallet-error-handling-design.md`.

mod config;
mod crypto;
mod root;
mod rpc;
mod storage;
mod sync;
mod transaction;
mod user_input;

#[cfg(test)]
mod status_mapping_tests;

pub use self::config::ConfigError;
pub use self::crypto::CryptoError;
pub use self::root::{WalletError, WalletResult};
pub use self::rpc::RpcError;
pub use self::storage::StorageError;
pub use self::sync::SyncError;
pub use self::transaction::TransactionError;
pub use self::user_input::UserInputError;
