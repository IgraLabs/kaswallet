use crate::error_location::ErrorLocation;
use kaspa_addresses::Prefix;
use thiserror::Error;

// User-facing message used for both `WrongPassword` and `KeyFileCorrupt`.
//
// Decryption failure (wrong password) and ciphertext tampering are
// indistinguishable from the user's perspective. We collapse them into the
// same string so an attacker cannot use the error oracle to learn whether a
// given keys file is genuine or has been tampered with — they can only learn
// that *something* is wrong with the (password, file) pair.
const KEY_DECRYPT_FAILED_MSG: &str = "wrong password or corrupt keys file";

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("{location} KeyFileNotFound: {path}")]
    KeyFileNotFound {
        path: String,
        location: ErrorLocation,
    },

    // Static-shape failures of the keys file itself — JSON parse errors,
    // unreadable contents, version mismatches. Distinct from `KeyFileCorrupt`,
    // which describes a file that decrypted-but-failed-validation.
    #[error("{location} KeyFileMalformed: path={path}, reason={reason}")]
    KeyFileMalformed {
        path: String,
        reason: String,
        location: ErrorLocation,
    },

    // The keys file was loaded and parsed, but ciphertext processing failed
    // for any reason other than the AEAD tag check (corrupt salt, bad hex,
    // non-UTF8 plaintext, invalid BIP39 mnemonic). Sibling to `WrongPassword`.
    #[error("{location} KeyFileCorrupt: {reason}")]
    KeyFileCorrupt {
        reason: String,
        location: ErrorLocation,
    },

    // AEAD decryption tag check failed. Indistinguishable from a tampered
    // ciphertext, so we never carry a `reason` here — the variant itself is
    // the only signal we expose to the caller.
    #[error("{location} WrongPassword")]
    WrongPassword { location: ErrorLocation },

    // Encryption-time failure (Argon2 hashing, AEAD encrypt). These should be
    // unreachable with valid inputs — surfacing them as a typed variant
    // instead of a panic preserves a clean error path for tests/fuzzing.
    #[error("{location} EncryptionFailed: {reason}")]
    EncryptionFailed {
        reason: String,
        location: ErrorLocation,
    },

    #[error("{location} Bip32Derivation: {reason}")]
    Bip32Derivation {
        reason: String,
        location: ErrorLocation,
    },

    #[error("{location} SignatureFailed: input_index={input_index}, reason={reason}")]
    SignatureFailed {
        input_index: usize,
        reason: String,
        location: ErrorLocation,
    },

    #[error("{location} PrefixMismatch: expected={expected:?}, got={got:?}")]
    PrefixMismatch {
        expected: Prefix,
        got: Prefix,
        location: ErrorLocation,
    },

    // Failures from `kaspa_txscript` (multisig redeem script construction,
    // script-pub-key → address conversion). Categorically separate from
    // `Bip32Derivation`, which is reserved for `kaspa_bip32` failures.
    #[error("{location} ScriptError: stage={stage}, reason={reason}")]
    ScriptError {
        stage: &'static str,
        reason: String,
        location: ErrorLocation,
    },
}

impl CryptoError {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::KeyFileNotFound { .. } => "KeyFileNotFound",
            Self::KeyFileMalformed { .. } => "KeyFileMalformed",
            Self::KeyFileCorrupt { .. } => "KeyFileCorrupt",
            Self::WrongPassword { .. } => "WrongPassword",
            Self::EncryptionFailed { .. } => "EncryptionFailed",
            Self::Bip32Derivation { .. } => "Bip32Derivation",
            Self::SignatureFailed { .. } => "SignatureFailed",
            Self::PrefixMismatch { .. } => "PrefixMismatch",
            Self::ScriptError { .. } => "ScriptError",
        }
    }

    pub fn location(&self) -> ErrorLocation {
        match self {
            Self::KeyFileNotFound { location, .. }
            | Self::KeyFileMalformed { location, .. }
            | Self::KeyFileCorrupt { location, .. }
            | Self::WrongPassword { location }
            | Self::EncryptionFailed { location, .. }
            | Self::Bip32Derivation { location, .. }
            | Self::SignatureFailed { location, .. }
            | Self::PrefixMismatch { location, .. }
            | Self::ScriptError { location, .. } => *location,
        }
    }

    pub fn user_message(&self) -> String {
        match self {
            Self::KeyFileNotFound { path, .. } => format!("keys file not found: {path}"),
            Self::KeyFileMalformed { path, reason, .. } => {
                format!("keys file is malformed at {path}: {reason}")
            }
            // Same string as WrongPassword — see KEY_DECRYPT_FAILED_MSG.
            Self::KeyFileCorrupt { .. } => KEY_DECRYPT_FAILED_MSG.to_string(),
            Self::WrongPassword { .. } => KEY_DECRYPT_FAILED_MSG.to_string(),
            Self::EncryptionFailed { .. } => "failed to encrypt mnemonic".to_string(),
            Self::Bip32Derivation { reason, .. } => format!("bip32 derivation failed: {reason}"),
            Self::SignatureFailed {
                input_index,
                reason,
                ..
            } => format!("signature failed at input {input_index}: {reason}"),
            Self::PrefixMismatch { expected, got, .. } => {
                format!("address prefix mismatch: expected {expected:?}, got {got:?}")
            }
            Self::ScriptError { stage, reason, .. } => {
                format!("script error at {stage}: {reason}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_mismatch_display() {
        let err = CryptoError::PrefixMismatch {
            expected: Prefix::Mainnet,
            got: Prefix::Testnet,
            location: ErrorLocation::capture(),
        };
        assert!(err.to_string().contains("PrefixMismatch"));
        assert_eq!(err.kind_name(), "PrefixMismatch");
    }

    #[test]
    fn wrong_password_and_corrupt_share_user_message() {
        let wrong = CryptoError::WrongPassword {
            location: ErrorLocation::capture(),
        };
        let corrupt = CryptoError::KeyFileCorrupt {
            reason: "AEAD tag mismatch".into(),
            location: ErrorLocation::capture(),
        };
        assert_eq!(wrong.user_message(), corrupt.user_message());
        assert!(!wrong.user_message().contains("crypto.rs"));
    }
}
