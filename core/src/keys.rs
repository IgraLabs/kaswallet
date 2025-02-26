use serde::{Deserialize, Serialize};
use std::io::Result;

#[derive(Serialize, Deserialize, Debug)]
pub struct Keys {
    pub version: u32,
    pub encrypted_mnemonics: Vec<EncryptedMnemonic>,
    pub public_keys: Vec<String>,

    pub last_used_exeternal_index: u64,
    pub last_used_internal_index: u64,

    pub minumum_signatures: u16,
    pub cosigner_index: u16,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EncryptedMnemonic {
    pub cipher: String,
    pub salt: String,
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
