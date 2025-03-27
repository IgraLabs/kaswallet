use crate::encrypted_mnemonic::EncryptedMnemonic;
use kaspa_bip32::secp256k1::PublicKey;
use kaspa_bip32::{ExtendedPublicKey, Mnemonic, Prefix};
use log::debug;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering::Relaxed;

pub const KEY_FILE_VERSION: i32 = 1;

#[derive(Debug)]
pub struct Keys {
    file_path: String,

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
    fn to_keys(&self, file_path: String, prefix: Prefix) -> Keys {
        let public_keys: Vec<ExtendedPublicKey<PublicKey>> = self
            .public_keys
            .iter()
            .map(|x| {
                debug!("Public Keys: {:?}", x);
                let x_public_key: ExtendedPublicKey<PublicKey> =
                    ExtendedPublicKey::from_str(x).unwrap();

                x_public_key
            })
            .collect();

        Keys {
            file_path,
            version: self.version.clone(),
            encrypted_mnemonics: self.encrypted_mnemonics.clone(),
            public_keys_prefix: prefix,
            public_keys,
            last_used_external_index: AtomicU32::new(self.last_used_external_index),
            last_used_internal_index: AtomicU32::new(self.last_used_internal_index),
            minimum_signatures: self.minimum_signatures,
            cosigner_index: self.cosigner_index,
        }
    }
}

impl Keys {
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

    pub fn load(file_path: String, prefix: Prefix) -> Result<Keys, Box<dyn Error + Send + Sync>> {
        let serialized = fs::read_to_string(&file_path)?;
        let keys_json: KeysJson = serde_json::from_str(&serialized)?;
        Ok(keys_json.to_keys(file_path, prefix))
    }

    pub fn save(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let keys_json: KeysJson = self.into();
        let serialized = serde_json::to_string_pretty(&keys_json)?;

        let path = Path::new(&self.file_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = File::create(path)?;

        file.write_all(serialized.as_bytes())?;

        Ok(())
    }

    pub fn decrypt_mnemonics(
        &self,
        password: &String,
    ) -> Result<Vec<Mnemonic>, Box<dyn Error + Send + Sync>> {
        let mut mnemonics = Vec::new();
        for encrypted_mnemonic in &self.encrypted_mnemonics {
            let mnemonic = encrypted_mnemonic.decrypt(password)?;
            mnemonics.push(mnemonic);
        }
        Ok(mnemonics)
    }
}
