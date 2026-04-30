use crate::encrypted_mnemonic::EncryptedMnemonic;
use crate::error_location::ErrorLocation;
use crate::errors::{CryptoError, StorageError, WalletResult};
use kaspa_bip32::secp256k1::PublicKey;
use kaspa_bip32::{DerivationPath, ExtendedPublicKey, Mnemonic, Prefix};
use log::debug;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering::Relaxed;

pub const KEY_FILE_VERSION: i32 = 1;

const SINGLE_SINGER_PURPOSE: u32 = 44;
const MULTISIG_PURPOSE: u32 = 45;
const KASPA_COIN_TYPE: u32 = 111111;

pub fn master_key_path(is_multisig: bool) -> DerivationPath {
    let purpose = if is_multisig {
        MULTISIG_PURPOSE
    } else {
        SINGLE_SINGER_PURPOSE
    };
    let path_string = format!("m/{}'/{}'/0'", purpose, KASPA_COIN_TYPE);
    // Path is built from `u32` constants we control; the format always parses.
    // If this ever fails, it is a programmer error — not a runtime input issue.
    DerivationPath::from_str(&path_string).expect("master_key_path is statically valid")
}

#[derive(Debug)]
pub struct Keys {
    pub file_path: String,

    pub version: i32,
    pub encrypted_mnemonics: Vec<EncryptedMnemonic>,
    public_keys_prefix: Prefix,
    pub public_keys: Vec<ExtendedPublicKey<PublicKey>>,

    pub last_used_external_index: AtomicU32,
    pub last_used_internal_index: AtomicU32,

    pub minimum_signatures: u16,
    pub cosigner_index: u16,
}

#[derive(Clone, Serialize, Deserialize)]
struct KeysJson {
    version: i32,
    encrypted_mnemonics: Vec<EncryptedMnemonic>,
    public_keys: Vec<String>,
    last_used_external_index: u32,
    last_used_internal_index: u32,
    minimum_signatures: u16,
    cosigner_index: u16,
}

impl From<&Keys> for KeysJson {
    fn from(keys: &Keys) -> Self {
        let public_keys: Vec<String> = keys
            .public_keys
            .iter()
            .map(|x| x.to_string(Some(keys.public_keys_prefix)))
            .collect();

        KeysJson {
            version: keys.version,
            encrypted_mnemonics: keys.encrypted_mnemonics.clone(),
            public_keys,
            last_used_external_index: keys.last_used_external_index.load(Relaxed),
            last_used_internal_index: keys.last_used_internal_index.load(Relaxed),
            minimum_signatures: keys.minimum_signatures,
            cosigner_index: keys.cosigner_index,
        }
    }
}

impl KeysJson {
    fn to_keys(&self, file_path: &str, prefix: Prefix) -> Result<Keys, CryptoError> {
        // A single malformed entry would have panicked the daemon at startup
        // (.unwrap()). Surface it as a typed `KeyFileMalformed` so callers can
        // render a meaningful error and exit cleanly instead of crashing.
        let public_keys = self
            .public_keys
            .iter()
            .map(|x| {
                debug!("Public Keys: {:?}", x);
                ExtendedPublicKey::<PublicKey>::from_str(x).map_err(|e| {
                    CryptoError::KeyFileMalformed {
                        path: file_path.to_string(),
                        reason: format!("invalid extended public key {x:?}: {e}"),
                        location: ErrorLocation::capture(),
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Keys {
            file_path: file_path.to_string(),
            version: self.version,
            encrypted_mnemonics: self.encrypted_mnemonics.clone(),
            public_keys_prefix: prefix,
            public_keys,
            last_used_external_index: AtomicU32::new(self.last_used_external_index),
            last_used_internal_index: AtomicU32::new(self.last_used_internal_index),
            minimum_signatures: self.minimum_signatures,
            cosigner_index: self.cosigner_index,
        })
    }
}

impl Keys {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        file_path: String,
        version: i32,
        encrypted_mnemonics: Vec<EncryptedMnemonic>,
        public_keys_prefix: Prefix,
        public_keys: Vec<ExtendedPublicKey<PublicKey>>,
        last_used_external_index: u32,
        last_used_internal_index: u32,
        minimum_signatures: u16,
        cosigner_index: u16,
    ) -> Self {
        Keys {
            file_path,
            version,
            encrypted_mnemonics,
            public_keys_prefix,
            public_keys,
            last_used_external_index: AtomicU32::new(last_used_external_index),
            last_used_internal_index: AtomicU32::new(last_used_internal_index),
            minimum_signatures,
            cosigner_index,
        }
    }

    pub fn load(file_path: &str, prefix: Prefix) -> Result<Keys, CryptoError> {
        let serialized = fs::read_to_string(file_path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => CryptoError::KeyFileNotFound {
                path: file_path.to_string(),
                location: ErrorLocation::capture(),
            },
            _ => CryptoError::KeyFileMalformed {
                path: file_path.to_string(),
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            },
        })?;
        let keys_json: KeysJson =
            serde_json::from_str(&serialized).map_err(|e| CryptoError::KeyFileMalformed {
                path: file_path.to_string(),
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            })?;
        keys_json.to_keys(file_path, prefix)
    }

    pub fn save(&self) -> WalletResult<()> {
        let keys_json: KeysJson = self.into();
        let serialized =
            serde_json::to_string_pretty(&keys_json).map_err(|e| StorageError::Serialize {
                kind: "keys.json",
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            })?;

        let path = Path::new(&self.file_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| StorageError::Io {
                path: parent.display().to_string(),
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            })?;
        }
        let mut file = File::create(path).map_err(|e| StorageError::Io {
            path: self.file_path.clone(),
            reason: e.to_string(),
            location: ErrorLocation::capture(),
        })?;

        file.write_all(serialized.as_bytes())
            .map_err(|e| StorageError::Io {
                path: self.file_path.clone(),
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            })?;

        Ok(())
    }

    pub fn decrypt_mnemonics(&self, password: &SecretString) -> WalletResult<Vec<Mnemonic>> {
        let mut mnemonics = Vec::new();
        for encrypted_mnemonic in &self.encrypted_mnemonics {
            let mnemonic = encrypted_mnemonic.decrypt(password)?;
            mnemonics.push(mnemonic);
        }
        Ok(mnemonics)
    }
}

#[cfg(test)]
mod keys_error_tests {
    use super::*;
    use kaspa_bip32::Prefix;

    #[test]
    fn load_returns_typed_error_when_file_missing() {
        let res = Keys::load("/nonexistent/path/keys.json", Prefix::KPUB);
        let err = res.unwrap_err();
        assert_eq!(err.kind_name(), "KeyFileNotFound", "got: {err}");
    }

    #[test]
    fn load_returns_malformed_when_pubkey_invalid() {
        use std::io::Write as _;
        let dir = std::env::temp_dir();
        let path = dir.join("kaswallet-keys-malformed-test.json");
        let mut f = std::fs::File::create(&path).unwrap();
        let bad_keys = serde_json::json!({
            "version": 1,
            "encrypted_mnemonics": [],
            "public_keys": ["not-an-xpub"],
            "last_used_external_index": 0,
            "last_used_internal_index": 0,
            "minimum_signatures": 1,
            "cosigner_index": 0,
        });
        f.write_all(bad_keys.to_string().as_bytes()).unwrap();
        drop(f);
        let res = Keys::load(path.to_str().unwrap(), Prefix::KPUB);
        let err = res.unwrap_err();
        assert_eq!(err.kind_name(), "KeyFileMalformed", "got: {err}");
        let _ = std::fs::remove_file(&path);
    }
}
