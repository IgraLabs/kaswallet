use crate::errors::WalletError::InternalServerError;
use crate::errors::{ResultExt, WalletResult};
use argon2::password_hash::{SaltString, rand_core::OsRng};
use argon2::{Argon2, PasswordHasher};
use chacha20poly1305::aead::{AeadMutInPlace, Key, Nonce};
use chacha20poly1305::{AeadCore, XChaCha20Poly1305, aead::KeyInit};
use kaspa_bip32::Language;
use kaspa_bip32::mnemonic::Mnemonic;
use serde::{Deserialize, Serialize};

const NONCE_SIZE: usize = 24;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct EncryptedMnemonic {
    cipher: String,
    salt: String,
}

impl EncryptedMnemonic {
    pub fn new(mnemonic: &Mnemonic, password: &String) -> WalletResult<Self> {
        let salt = SaltString::generate(&mut OsRng);
        let cipher = Self::encrypt(mnemonic, password, &salt)?;

        Ok(EncryptedMnemonic {
            cipher: hex::encode(cipher),
            salt: salt.to_string(),
        })
    }

    // Key::<XChaCha20Poly1305>::from_slice uses a deprecated method from a dependency
    #[allow(deprecated)]
    pub fn decrypt(&self, password: &String) -> WalletResult<Mnemonic> {
        let salt = SaltString::from_b64(&self.salt).to_wallet_result_internal()?;
        let argon2 = Argon2::default();
        let password_hash = argon2
            .hash_password(password.as_bytes(), &salt)
            .to_wallet_result_internal()?;
        let hash = password_hash.hash.unwrap();
        let key_bytes = hash.as_bytes();
        let key = Key::<XChaCha20Poly1305>::from_slice(key_bytes);
        let mut cipher = XChaCha20Poly1305::new(key);

        let cipher_bytes = hex::decode(&self.cipher).to_wallet_result_internal()?;
        let (nonce_bytes, cipher_text) = cipher_bytes.split_at(NONCE_SIZE);
        let mut cipher_text = cipher_text.to_vec();
        let nonce = Nonce::<XChaCha20Poly1305>::from_slice(nonce_bytes);
        cipher
            .decrypt_in_place(nonce, &[], &mut cipher_text)
            .map_err(|e| InternalServerError(format!("Decryption failed: {}", e)))?;
        let mnemonic_string = String::from_utf8(cipher_text).to_wallet_result_internal()?;

        Mnemonic::new(mnemonic_string, Language::English).to_wallet_result_internal()
    }

    // Key::<XChaCha20Poly1305>::from_slice uses a deprecated method from a dependency
    #[allow(deprecated)]
    fn encrypt(mnemonic: &Mnemonic, password: &String, salt: &SaltString) -> WalletResult<Vec<u8>> {
        let argon2 = Argon2::default();
        let password_hash = argon2
            .hash_password(password.as_bytes(), salt)
            .to_wallet_result_internal()?;
        let hash = password_hash.hash.unwrap();
        let key_bytes = hash.as_bytes();
        let key = Key::<XChaCha20Poly1305>::from_slice(key_bytes);
        let mut cipher = XChaCha20Poly1305::new(key);
        let nonce = XChaCha20Poly1305::generate_nonce(OsRng);

        let mut buffer = mnemonic.phrase().as_bytes().to_vec();
        buffer.reserve(NONCE_SIZE);
        cipher
            .encrypt_in_place(&nonce, &[], &mut buffer)
            .map_err(|e| InternalServerError(format!("Encryption failed: {}", e)))?;
        buffer.splice(0..0, nonce.iter().cloned());

        Ok(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_bip32::mnemonic::Mnemonic;
    use kaspa_bip32::{Language, WordCount};
    use kaswallet_test_helpers::mnemonics;
    use rstest::rstest;

    #[rstest]
    #[case(WordCount::Words12)]
    #[case(WordCount::Words24)]
    fn test_encrypt_decrypt_roundtrip(#[case] word_count: WordCount) {
        let mnemonic = Mnemonic::random(word_count, Language::English).unwrap();
        let password = "test_password".to_string();

        let encrypted =
            EncryptedMnemonic::new(&mnemonic, &password).expect("Encryption should succeed");

        let decrypted = encrypted
            .decrypt(&password)
            .expect("Decryption should succeed");

        assert_eq!(
            mnemonic.phrase(),
            decrypted.phrase(),
            "Decrypted mnemonic should match original"
        );
    }

    #[rstest]
    #[case("normal_password")]
    #[case("")]
    #[case("with spaces and 特殊字符!@#$%^&*()")]
    #[case("password_with_emojis_🔐🔑💎")]
    #[case(&"x".repeat(1000))]
    fn test_password_variants(#[case] password: &str) {
        let mnemonic = mnemonics::create_known_test_mnemonic();
        let password = password.to_string();

        let encrypted =
            EncryptedMnemonic::new(&mnemonic, &password).expect("Encryption should succeed");

        let decrypted = encrypted
            .decrypt(&password)
            .expect("Decryption should succeed");

        assert_eq!(
            mnemonic.phrase(),
            decrypted.phrase(),
            "Decrypted mnemonic should match original for password variant"
        );
    }

    #[test]
    fn test_wrong_password_fails() {
        let mnemonic = mnemonics::create_known_test_mnemonic();
        let correct_password = "correct_password".to_string();
        let wrong_password = "wrong_password".to_string();

        let encrypted = EncryptedMnemonic::new(&mnemonic, &correct_password)
            .expect("Encryption should succeed");

        let result = encrypted.decrypt(&wrong_password);

        assert!(
            result.is_err(),
            "Decryption with wrong password should fail"
        );

        if let Err(e) = result {
            let error_msg = format!("{:?}", e);
            assert!(
                error_msg.contains("Decryption failed"),
                "Error should mention decryption failure, got: {}",
                error_msg
            );
        }
    }

    #[test]
    fn test_randomness() {
        let mnemonic = mnemonics::create_known_test_mnemonic();
        let password = "same_password".to_string();

        let encrypted1 =
            EncryptedMnemonic::new(&mnemonic, &password).expect("First encryption should succeed");

        let encrypted2 =
            EncryptedMnemonic::new(&mnemonic, &password).expect("Second encryption should succeed");

        assert_ne!(
            encrypted1.cipher, encrypted2.cipher,
            "Cipher text should be different due to random nonce"
        );

        assert_ne!(
            encrypted1.salt, encrypted2.salt,
            "Salt should be different due to randomness"
        );

        // Both should decrypt to same mnemonic
        let decrypted1 = encrypted1.decrypt(&password).unwrap();
        let decrypted2 = encrypted2.decrypt(&password).unwrap();

        assert_eq!(decrypted1.phrase(), decrypted2.phrase());
        assert_eq!(decrypted1.phrase(), mnemonic.phrase());
    }

    #[rstest]
    #[case("ZZZZ", "valid")]
    #[case("not_hex_!!!!", "valid")]
    fn test_corrupted_cipher_fails(#[case] bad_cipher: &str, #[case] _desc: &str) {
        let mnemonic = mnemonics::create_known_test_mnemonic();
        let password = "password".to_string();

        let mut encrypted =
            EncryptedMnemonic::new(&mnemonic, &password).expect("Encryption should succeed");

        encrypted.cipher = bad_cipher.to_string();

        let result = encrypted.decrypt(&password);
        assert!(
            result.is_err(),
            "Decryption should fail with corrupted cipher: {}",
            bad_cipher
        );
    }

    #[rstest]
    #[case("invalid!!!base64")]
    #[case("@#$%")]
    fn test_corrupted_salt_fails(#[case] bad_salt: &str) {
        let mnemonic = mnemonics::create_known_test_mnemonic();
        let password = "password".to_string();

        let mut encrypted =
            EncryptedMnemonic::new(&mnemonic, &password).expect("Encryption should succeed");

        encrypted.salt = bad_salt.to_string();

        let result = encrypted.decrypt(&password);
        assert!(
            result.is_err(),
            "Decryption should fail with corrupted salt: {}",
            bad_salt
        );
    }
}
