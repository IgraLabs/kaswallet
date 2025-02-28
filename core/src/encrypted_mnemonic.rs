use argon2::password_hash::{rand_core::OsRng, SaltString};
use argon2::{Argon2, PasswordHasher};
use chacha20poly1305::aead::{AeadMutInPlace, Key};
use chacha20poly1305::{aead::KeyInit, AeadCore, XChaCha20Poly1305};
use kaspa_bip32::mnemonic::Mnemonic;

#[derive(Debug)]
pub struct EncryptedMnemonic {
    cipher: String,
    salt: String,
}

impl EncryptedMnemonic {
    pub fn new(mnemonic: &Mnemonic, password: &String) -> Self {
        let salt = SaltString::generate(&mut OsRng);
        let cipher = encrypt_mnemonic(mnemonic, password, &salt);

        EncryptedMnemonic {
            cipher: hex::encode(cipher),
            salt: hex::encode(salt.to_string()),
        }
    }
}

fn encrypt_mnemonic(mnemonic: &Mnemonic, password: &String, salt: &SaltString) -> Vec<u8> {
    let argon2 = Argon2::default();
    let password_hash = argon2.hash_password(password.as_bytes(), salt).unwrap();
    let hash = password_hash.hash.unwrap();
    let key_bytes = hash.as_bytes();
    let key = Key::<XChaCha20Poly1305>::from_slice(key_bytes);
    let mut cipher = XChaCha20Poly1305::new(&key);
    let nonce = XChaCha20Poly1305::generate_nonce(OsRng);

    let mut buffer = mnemonic.phrase().as_bytes().to_vec();
    buffer.reserve(16);
    cipher.encrypt_in_place(&nonce, &[], &mut buffer).unwrap();
    buffer.splice(0..0, nonce.iter().cloned());

    buffer
}
