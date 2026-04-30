use crate::error_location::ErrorLocation;
use crate::errors::{CryptoError, WalletResult};
use argon2::password_hash::{SaltString, rand_core::OsRng};
use argon2::{Argon2, PasswordHasher};
use chacha20poly1305::aead::{AeadMutInPlace, Key, Nonce};
use chacha20poly1305::{AeadCore, XChaCha20Poly1305, aead::KeyInit};
use kaspa_bip32::Language;
use kaspa_bip32::mnemonic::Mnemonic;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

const NONCE_SIZE: usize = 24;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct EncryptedMnemonic {
    cipher: String,
    salt: String,
}

impl EncryptedMnemonic {
    pub fn new(mnemonic: &Mnemonic, password: &SecretString) -> WalletResult<Self> {
        let salt = SaltString::generate(&mut OsRng);
        let cipher = Self::encrypt(mnemonic, password, &salt)?;

        Ok(EncryptedMnemonic {
            cipher: hex::encode(cipher),
            salt: salt.to_string(),
        })
    }

    // Key::<XChaCha20Poly1305>::from_slice uses a deprecated method from a dependency
    #[allow(deprecated)]
    pub fn decrypt(&self, password: &SecretString) -> WalletResult<Mnemonic> {
        // Static-shape failures of the keys file (corrupt salt, bad hex, bad
        // plaintext, invalid mnemonic) are mapped to `KeyFileCorrupt`. The AEAD
        // tag check below maps to `WrongPassword`. Both variants share an
        // identical user-facing message — see `crypto::KEY_DECRYPT_FAILED_MSG`.
        let salt = SaltString::from_b64(&self.salt).map_err(|e| CryptoError::KeyFileCorrupt {
            reason: e.to_string(),
            location: ErrorLocation::capture(),
        })?;
        let argon2 = Argon2::default();
        let password_hash = argon2
            .hash_password(password.expose_secret().as_bytes(), &salt)
            .map_err(|e| CryptoError::KeyFileCorrupt {
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            })?;
        let hash = password_hash.hash.unwrap();
        let key_bytes = hash.as_bytes();
        let key = Key::<XChaCha20Poly1305>::from_slice(key_bytes);
        let mut cipher = XChaCha20Poly1305::new(key);

        let cipher_bytes = hex::decode(&self.cipher).map_err(|e| CryptoError::KeyFileCorrupt {
            reason: e.to_string(),
            location: ErrorLocation::capture(),
        })?;
        let (nonce_bytes, cipher_text) = cipher_bytes.split_at(NONCE_SIZE);
        let mut cipher_text = cipher_text.to_vec();
        let nonce = Nonce::<XChaCha20Poly1305>::from_slice(nonce_bytes);
        // AEAD tag mismatch — canonically "wrong password", but tampering looks
        // identical from here. The variant carries no `reason` so we never leak
        // a distinguishable string to an attacker holding the keys file.
        cipher
            .decrypt_in_place(nonce, &[], &mut cipher_text)
            .map_err(|_| CryptoError::WrongPassword {
                location: ErrorLocation::capture(),
            })?;
        let mnemonic_string =
            String::from_utf8(cipher_text).map_err(|e| CryptoError::KeyFileCorrupt {
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            })?;

        Mnemonic::new(mnemonic_string, Language::English).map_err(|e| {
            CryptoError::KeyFileCorrupt {
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            }
            .into()
        })
    }

    // Key::<XChaCha20Poly1305>::from_slice uses a deprecated method from a dependency
    #[allow(deprecated)]
    fn encrypt(
        mnemonic: &Mnemonic,
        password: &SecretString,
        salt: &SaltString,
    ) -> WalletResult<Vec<u8>> {
        let argon2 = Argon2::default();
        let password_hash = argon2
            .hash_password(password.expose_secret().as_bytes(), salt)
            .map_err(|e| CryptoError::EncryptionFailed {
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            })?;
        let hash = password_hash.hash.unwrap();
        let key_bytes = hash.as_bytes();
        let key = Key::<XChaCha20Poly1305>::from_slice(key_bytes);
        let mut cipher = XChaCha20Poly1305::new(key);
        let nonce = XChaCha20Poly1305::generate_nonce(OsRng);

        let mut buffer = mnemonic.phrase().as_bytes().to_vec();
        buffer.reserve(NONCE_SIZE);
        cipher
            .encrypt_in_place(&nonce, &[], &mut buffer)
            .map_err(|e| CryptoError::EncryptionFailed {
                reason: e.to_string(),
                location: ErrorLocation::capture(),
            })?;
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

    fn secret(s: &str) -> SecretString {
        SecretString::from(s.to_string())
    }

    // Mnemonic doesn't impl Debug, so .unwrap_err() doesn't compile.
    fn expect_err<T>(r: WalletResult<T>) -> crate::errors::WalletError {
        match r {
            Ok(_) => panic!("expected error, got Ok"),
            Err(e) => e,
        }
    }

    #[rstest]
    #[case(WordCount::Words12)]
    #[case(WordCount::Words24)]
    fn test_encrypt_decrypt_roundtrip(#[case] word_count: WordCount) {
        let mnemonic = Mnemonic::random(word_count, Language::English).unwrap();
        let password = secret("test_password");

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
        let password = secret(password);

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
    fn test_wrong_password_fails_with_typed_variant() {
        let mnemonic = mnemonics::create_known_test_mnemonic();
        let correct = secret("correct_password");
        let wrong = secret("wrong_password");

        let encrypted = EncryptedMnemonic::new(&mnemonic, &correct).unwrap();

        let err = expect_err(encrypted.decrypt(&wrong));
        assert_eq!(err.kind_name(), "WrongPassword", "got: {err}");
    }

    #[test]
    fn wrong_password_user_message_matches_corrupt() {
        // Same user-facing string for both — confirms the oracle is closed.
        let mnemonic = mnemonics::create_known_test_mnemonic();
        let correct = secret("correct_password");
        let wrong = secret("wrong_password");

        let encrypted = EncryptedMnemonic::new(&mnemonic, &correct).unwrap();
        let wrong_err = expect_err(encrypted.decrypt(&wrong));

        let mut corrupted = encrypted;
        corrupted.cipher = "ZZZZ".into();
        let corrupt_err = expect_err(corrupted.decrypt(&correct));

        assert_eq!(wrong_err.user_message(), corrupt_err.user_message());
    }

    #[test]
    fn test_randomness() {
        let mnemonic = mnemonics::create_known_test_mnemonic();
        let password = secret("same_password");

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

        let decrypted1 = encrypted1.decrypt(&password).unwrap();
        let decrypted2 = encrypted2.decrypt(&password).unwrap();

        assert_eq!(decrypted1.phrase(), decrypted2.phrase());
        assert_eq!(decrypted1.phrase(), mnemonic.phrase());
    }

    #[rstest]
    #[case("ZZZZ", "valid")]
    #[case("not_hex_!!!!", "valid")]
    fn test_corrupted_cipher_fails_with_corrupt_variant(
        #[case] bad_cipher: &str,
        #[case] _desc: &str,
    ) {
        let mnemonic = mnemonics::create_known_test_mnemonic();
        let password = secret("password");

        let mut encrypted =
            EncryptedMnemonic::new(&mnemonic, &password).expect("Encryption should succeed");

        encrypted.cipher = bad_cipher.to_string();

        let err = expect_err(encrypted.decrypt(&password));
        assert_eq!(err.kind_name(), "KeyFileCorrupt", "got: {err}");
    }

    #[rstest]
    #[case("invalid!!!base64")]
    #[case("@#$%")]
    fn test_corrupted_salt_fails_with_corrupt_variant(#[case] bad_salt: &str) {
        let mnemonic = mnemonics::create_known_test_mnemonic();
        let password = secret("password");

        let mut encrypted =
            EncryptedMnemonic::new(&mnemonic, &password).expect("Encryption should succeed");

        encrypted.salt = bad_salt.to_string();

        let err = expect_err(encrypted.decrypt(&password));
        assert_eq!(err.kind_name(), "KeyFileCorrupt", "got: {err}");
    }
}
