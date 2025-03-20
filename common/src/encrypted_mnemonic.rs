use argon2::password_hash::{rand_core::OsRng, SaltString};
use argon2::{Argon2, PasswordHasher};
use chacha20poly1305::aead::{AeadMutInPlace, Key};
use chacha20poly1305::{aead::KeyInit, AeadCore, XChaCha20Poly1305};
use kaspa_bip32::mnemonic::Mnemonic;
use kaspa_bip32::Language;
use serde::{Deserialize, Serialize};
use std::error::Error;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct EncryptedMnemonic {
    cipher: String,
    salt: String,
}

impl EncryptedMnemonic {
    pub fn new(
        mnemonic: &Mnemonic,
        password: &String,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let salt = SaltString::generate(&mut OsRng);
        let cipher = Self::encrypt(mnemonic, password, &salt)?;

        Ok(EncryptedMnemonic {
            cipher: hex::encode(cipher),
            salt: salt.to_string(),
        })
    }

    pub fn decrypt(&self, password: &String) -> Result<Mnemonic, Box<dyn Error + Send + Sync>> {
        let salt = SaltString::from_b64(&self.salt)?;
        let argon2 = Argon2::default();
        let password_hash = argon2.hash_password(password.as_bytes(), &salt)?;
        let hash = password_hash.hash.unwrap();
        let key_bytes = hash.as_bytes();
        let key = Key::<XChaCha20Poly1305>::from_slice(key_bytes);
        let mut cipher = XChaCha20Poly1305::new(&key);

        let mut cipher_bytes = hex::decode(&self.cipher)?;
        let nonce = XChaCha20Poly1305::generate_nonce(OsRng);
        cipher
            .decrypt_in_place(&nonce, &[], &mut cipher_bytes)
            .map_err(|e| format!("Decryption failed: {}", e))?;
        let mnemonic_string = String::from_utf8(cipher_bytes)?;

        let mnemonic = Mnemonic::new(mnemonic_string, Language::English)?;
        Ok(mnemonic)
    }

    fn encrypt(
        mnemonic: &Mnemonic,
        password: &String,
        salt: &SaltString,
    ) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        let argon2 = Argon2::default();
        let password_hash = argon2.hash_password(password.as_bytes(), salt)?;
        let hash = password_hash.hash.unwrap();
        let key_bytes = hash.as_bytes();
        let key = Key::<XChaCha20Poly1305>::from_slice(key_bytes);
        let mut cipher = XChaCha20Poly1305::new(&key);
        let nonce = XChaCha20Poly1305::generate_nonce(OsRng);

        let mut buffer = mnemonic.phrase().as_bytes().to_vec();
        buffer.reserve(16);
        cipher
            .encrypt_in_place(&nonce, &[], &mut buffer)
            .map_err(|e| format!("Encryption failed: {}", e))?;
        buffer.splice(0..0, nonce.iter().cloned());

        Ok(buffer)
    }
}
