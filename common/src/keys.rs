use crate::encrypted_mnemonic::EncryptedMnemonic;
use crate::error_location::ErrorLocation;
use crate::errors::{CryptoError, StorageError, WalletResult};
use kaspa_bip32::secp256k1::PublicKey;
use kaspa_bip32::{DerivationPath, ExtendedPublicKey, Mnemonic, Prefix};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::fs;
#[cfg(unix)]
use std::fs::File;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering::Relaxed;
use tempfile::{Builder, NamedTempFile};
use tracing::debug;
#[cfg(unix)]
use tracing::warn;

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

    /// Persist the key file atomically and durably.
    ///
    /// The previous implementation opened the live path with `File::create`,
    /// which truncates it to zero length, and then wrote in place — so a crash
    /// or kill between the truncation and a durable write could leave the file
    /// empty or partial. That is the failure mode behind the RPC-wallet
    /// key-loss incident. Instead we write the new contents to a uniquely named
    /// temporary file in the *same* directory, flush it, and atomically rename
    /// it over the target. Any failure before the rename leaves the prior
    /// snapshot byte-for-byte intact.
    ///
    /// Concurrency: `save` takes `&self` and holds no lock. Callers must
    /// serialize saves of a given key file (one live writer per path); the
    /// daemon does this by only ever saving while holding its
    /// `Arc<Mutex<AddressManager>>`. The atomic rename still guarantees every
    /// reader observes a complete old-or-new file even if that contract is
    /// violated.
    pub fn save(&self) -> WalletResult<()> {
        let keys_json: KeysJson = self.into();
        let serialized =
            serde_json::to_string_pretty(&keys_json).map_err(|e| StorageError::Serialize {
                kind: "keys.json",
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            })?;

        let path = Path::new(&self.file_path);
        let parent = Self::keys_parent(path);

        fs::create_dir_all(parent).map_err(|e| StorageError::Io {
            path: parent.display().to_string(),
            reason: format!("create keys directory: {e}"),
            location: ErrorLocation::capture(),
        })?;

        // The atomic rename replaces a directory entry, so the target must be a
        // regular file. `File::create` would instead follow a final-component
        // symlink and update its referent; renaming over the link would
        // silently abandon that referent and could move wallet material onto an
        // unexpected filesystem. Reject it explicitly.
        if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(StorageError::Io {
                path: self.file_path.clone(),
                reason: "keys file must be a regular file, not a symlink".to_string(),
                location: ErrorLocation::capture(),
            }
            .into());
        }

        let temp_file = Self::prepare_temp_file(path, parent, serialized.as_bytes())?;
        Self::commit_temp_file(temp_file, path, parent)
    }

    /// Directory that contains the key file, normalizing a bare filename's
    /// empty parent (`Path::new("keys.json").parent()` is `Some("")`, not
    /// `None`) to `.` so the temp file, rename, and directory fsync all resolve
    /// against a real directory.
    fn keys_parent(path: &Path) -> &Path {
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
    }

    /// Write `contents` to a fresh, uniquely named temporary file in `parent`,
    /// apply the target's permissions, and flush it to disk — everything up to
    /// but excluding the atomic rename. Split out from [`Keys::save`] so tests
    /// can drive a save to the exact post-`fsync`/pre-rename point (the window
    /// that reproduces the incident) and assert the live file is untouched.
    /// Production always follows this with [`Keys::commit_temp_file`].
    fn prepare_temp_file(
        path: &Path,
        parent: &Path,
        contents: &[u8],
    ) -> WalletResult<NamedTempFile> {
        #[cfg(not(unix))]
        let _ = path;

        // Same-directory temp guarantees a same-filesystem (non-`EXDEV`)
        // rename; the recognizable prefix lets operators spot temps orphaned by
        // a hard kill (they are never loaded and never overwrite the target).
        let mut temp_file = Builder::new()
            .prefix(".keys.json.tmp-")
            .tempfile_in(parent)
            .map_err(|e| StorageError::Io {
                path: parent.display().to_string(),
                reason: format!("create keys temp file: {e}"),
                location: ErrorLocation::capture(),
            })?;

        temp_file
            .write_all(contents)
            .map_err(|e| StorageError::Io {
                path: temp_file.path().display().to_string(),
                reason: format!("write keys temp file: {e}"),
                location: ErrorLocation::capture(),
            })?;

        // Set permissions before the durability barrier so the mode is part of
        // what `sync_all` flushes. Preserve an existing key file's mode
        // (operators may deliberately widen it, e.g. `0o640` for a group
        // reader); default a newly created file to owner-only `0o600`.
        #[cfg(unix)]
        {
            let mode = match fs::metadata(path) {
                Ok(metadata) => metadata.permissions().mode() & 0o777,
                Err(_) => 0o600,
            };
            temp_file
                .as_file()
                .set_permissions(fs::Permissions::from_mode(mode))
                .map_err(|e| StorageError::Io {
                    path: temp_file.path().display().to_string(),
                    reason: format!("set keys temp permissions: {e}"),
                    location: ErrorLocation::capture(),
                })?;
        }

        temp_file
            .as_file()
            .sync_all()
            .map_err(|e| StorageError::Io {
                path: temp_file.path().display().to_string(),
                reason: format!("fsync keys temp file: {e}"),
                location: ErrorLocation::capture(),
            })?;

        Ok(temp_file)
    }

    /// Atomically replace `path` with the prepared `temp_file`, then
    /// best-effort fsync `parent` so the rename itself is durable.
    fn commit_temp_file(temp_file: NamedTempFile, path: &Path, parent: &Path) -> WalletResult<()> {
        #[cfg(not(unix))]
        let _ = parent;

        // On failure the returned `PersistError` owns the temp file, so
        // dropping it here unlinks it — no manual cleanup needed.
        temp_file.persist(path).map_err(|e| StorageError::Io {
            path: path.display().to_string(),
            reason: format!("persist keys file: {}", e.error),
            location: ErrorLocation::capture(),
        })?;

        // Make the rename durable by fsyncing the directory. This is
        // best-effort: after a successful `persist` the new file is already
        // complete and visible, and propagating an error here would crash the
        // daemon's sync loop (which panics on any save error) over a
        // post-commit durability nuance rather than a data-integrity problem.
        #[cfg(unix)]
        match File::open(parent) {
            Ok(dir) => {
                if let Err(e) = dir.sync_all() {
                    warn!(
                        directory = %parent.display(),
                        error = %e,
                        "failed to fsync keys directory after save; file is written but its directory entry may not be durable until the next sync"
                    );
                }
            }
            Err(e) => warn!(
                directory = %parent.display(),
                error = %e,
                "failed to open keys directory to fsync after save"
            ),
        }

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

#[cfg(test)]
mod keys_atomic_save_tests {
    use super::*;
    use kaspa_bip32::Prefix;
    use std::path::Path;
    use std::sync::atomic::Ordering::Relaxed;

    /// Minimal `Keys` with no mnemonics/public keys — enough to exercise the
    /// persistence path, whose behavior is independent of key material.
    fn test_keys(path: &str, external: u32, internal: u32) -> Keys {
        Keys::new(
            path.to_string(),
            KEY_FILE_VERSION,
            Vec::new(),
            Prefix::KPUB,
            Vec::new(),
            external,
            internal,
            1,
            0,
        )
    }

    fn has_temp_file(dir: &Path) -> bool {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".keys.json.tmp-")
            })
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        let path_str = path.to_str().unwrap();

        test_keys(path_str, 7, 3).save().unwrap();

        let loaded = Keys::load(path_str, Prefix::KPUB).unwrap();
        assert_eq!(loaded.last_used_external_index.load(Relaxed), 7);
        assert_eq!(loaded.last_used_internal_index.load(Relaxed), 3);
        assert_eq!(loaded.version, KEY_FILE_VERSION);
    }

    /// The incident regression: a save interrupted after the temp file is
    /// written and fsynced but before the atomic rename must leave the previous
    /// valid snapshot byte-for-byte intact (never a 0-byte file).
    #[test]
    fn interrupted_save_after_fsync_preserves_prior_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        let path_str = path.to_str().unwrap();

        test_keys(path_str, 1, 1).save().unwrap();
        let original = std::fs::read(&path).unwrap();
        assert!(!original.is_empty());

        // Drive a newer save through the full prepare (write + perms + fsync),
        // then abandon it before commit — exactly what a kill mid-save does.
        let newer = test_keys(path_str, 9, 9);
        let keys_json: KeysJson = (&newer).into();
        let serialized = serde_json::to_string_pretty(&keys_json).unwrap();
        let target = Path::new(path_str);
        let parent = Keys::keys_parent(target);
        let temp = Keys::prepare_temp_file(target, parent, serialized.as_bytes()).unwrap();
        drop(temp);

        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "interrupted save corrupted the key file"
        );
        let loaded = Keys::load(path_str, Prefix::KPUB).unwrap();
        assert_eq!(loaded.last_used_external_index.load(Relaxed), 1);
        assert!(!has_temp_file(dir.path()), "temp left behind after drop");
    }

    #[test]
    fn successful_save_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");

        test_keys(path.to_str().unwrap(), 1, 1).save().unwrap();

        assert!(!has_temp_file(dir.path()), "temp file left after save");
        let entries = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(entries, 1, "expected only keys.json in the directory");
    }

    #[test]
    fn keys_parent_normalizes_bare_filename() {
        assert_eq!(Keys::keys_parent(Path::new("keys.json")), Path::new("."));
        assert_eq!(
            Keys::keys_parent(Path::new("/a/b/keys.json")),
            Path::new("/a/b")
        );
        assert_eq!(
            Keys::keys_parent(Path::new("relative/keys.json")),
            Path::new("relative")
        );
    }

    #[test]
    fn concurrent_saves_never_tear_the_file() {
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        let path_str = path.to_str().unwrap().to_string();
        test_keys(&path_str, 0, 0).save().unwrap();

        let keys = Arc::new(test_keys(&path_str, 0, 0));
        let handles: Vec<_> = (1..=8u32)
            .map(|i| {
                let keys = Arc::clone(&keys);
                std::thread::spawn(move || {
                    keys.last_used_external_index.store(i, Relaxed);
                    keys.save().unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        // Regardless of ordering, the atomic rename guarantees a complete,
        // valid snapshot — never a torn file.
        let loaded = Keys::load(&path_str, Prefix::KPUB).unwrap();
        assert!(loaded.last_used_external_index.load(Relaxed) <= 8);
        assert!(!has_temp_file(dir.path()));
    }

    #[cfg(unix)]
    #[test]
    fn new_key_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");

        test_keys(path.to_str().unwrap(), 0, 0).save().unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "new key file should be 0600, got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn existing_permissions_are_preserved() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        let path_str = path.to_str().unwrap();

        test_keys(path_str, 0, 0).save().unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();

        // A subsequent save must keep the operator-chosen 0640, not force 0600.
        test_keys(path_str, 5, 0).save().unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o640,
            "existing mode should be preserved, got {mode:o}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_into_read_only_dir_preserves_existing_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let keydir = dir.path().join("keys");
        std::fs::create_dir(&keydir).unwrap();
        let path = keydir.join("keys.json");
        let path_str = path.to_str().unwrap();

        test_keys(path_str, 2, 2).save().unwrap();
        let original = std::fs::read(&path).unwrap();

        std::fs::set_permissions(&keydir, std::fs::Permissions::from_mode(0o500)).unwrap();
        // If the OS won't enforce the restriction (e.g. running as root in CI),
        // skip the assertion rather than flake.
        let enforced = std::fs::File::create(keydir.join(".probe")).is_err();

        let result = test_keys(path_str, 8, 8).save();

        // Restore write permission so tempdir cleanup can run.
        std::fs::set_permissions(&keydir, std::fs::Permissions::from_mode(0o700)).unwrap();

        if enforced {
            assert!(result.is_err(), "save into a read-only dir should fail");
            assert_eq!(
                std::fs::read(&path).unwrap(),
                original,
                "read-only save corrupted the existing file"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_target_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real-keys.json");
        let link = dir.path().join("keys.json");

        test_keys(real.to_str().unwrap(), 4, 4).save().unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let err = test_keys(link.to_str().unwrap(), 9, 9).save().unwrap_err();
        assert!(
            format!("{err}").contains("regular file"),
            "expected a symlink rejection, got: {err}"
        );
        // The real file behind the link is untouched.
        let loaded = Keys::load(real.to_str().unwrap(), Prefix::KPUB).unwrap();
        assert_eq!(loaded.last_used_external_index.load(Relaxed), 4);
    }
}
