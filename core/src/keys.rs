use crate::encrypted_mnemonic::EncryptedMnemonic;
use serde::{Deserialize, Serialize};
use std::io::Result;

pub const KEY_FILE_VERSION: i32 = 1;

#[derive(Serialize, Deserialize, Debug)]
pub struct Keys {
    pub version: i32,
    pub encrypted_mnemonics: Vec<EncryptedMnemonic>,
    pub public_keys: Vec<String>,

    pub last_used_external_index: i64,
    pub last_used_internal_index: i64,

    pub minumum_signatures: i16,
    pub cosigner_index: i16,
}

pub fn save_keys(keys: &Keys, path: String) -> Result<()> {
    let serialized = serde_json::to_string(keys)?;
    std::fs::write(path, serialized)
}

pub fn load_keys(path: String) -> Result<Keys> {
    let serialized = std::fs::read_to_string(path)?;
    let keys: Keys = serde_json::from_str(&serialized)?;
    Ok(keys)
}
