//! Pure encrypt/decrypt logic: Argon2id key derivation + AES-256-GCM. No
//! file I/O, no terminal I/O - kept that way so it's directly unit-testable.
//! Operates on an arbitrary byte payload (a serialized [`crate::KrakenCredentials`]
//! in practice), so the crypto itself has no knowledge of what it's protecting.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rand::RngCore;
use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroizing;

use crate::error::CredentialsError;
use crate::keyfile::{KdfParams, KeyFile, CURRENT_VERSION};

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

fn derive_key(
    passphrase: &SecretString,
    salt: &[u8],
    params: &KdfParams,
) -> Result<Zeroizing<[u8; KEY_LEN]>, CredentialsError> {
    let argon2_params = Params::new(params.m_cost_kib, params.t_cost, params.p_cost, Some(KEY_LEN))
        .map_err(|e| CredentialsError::Kdf(e.to_string()))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon2_params);

    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    argon2
        .hash_password_into(passphrase.expose_secret().as_bytes(), salt, out.as_mut())
        .map_err(|e| CredentialsError::Kdf(e.to_string()))?;
    Ok(out)
}

/// Encrypt an arbitrary plaintext payload under `passphrase`, producing a
/// [`KeyFile`] ready to write to disk. A fresh random salt and nonce are
/// generated on every call.
pub fn encrypt_payload(passphrase: &SecretString, payload: &[u8]) -> Result<KeyFile, CredentialsError> {
    let params = KdfParams::default();

    let mut salt = [0u8; SALT_LEN];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);

    let key = derive_key(passphrase, &salt, &params)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_ref()));
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, payload)
        .map_err(|_| CredentialsError::Kdf("AES-GCM encryption failed".into()))?;

    Ok(KeyFile {
        version: CURRENT_VERSION,
        kdf: "argon2id".into(),
        kdf_params: params,
        salt: STANDARD.encode(salt),
        cipher: "aes-256-gcm".into(),
        nonce: STANDARD.encode(nonce_bytes),
        ciphertext: STANDARD.encode(ciphertext),
    })
}

/// Decrypt a [`KeyFile`] back into its raw plaintext payload. Any
/// authentication-tag failure - wrong passphrase or a corrupted file -
/// surfaces as `CredentialsError::DecryptionFailed`, never as garbage
/// plaintext, because AES-GCM is authenticated: a wrong key fails the tag
/// check before any plaintext bytes are returned.
pub fn decrypt_payload(passphrase: &SecretString, keyfile: &KeyFile) -> Result<Zeroizing<Vec<u8>>, CredentialsError> {
    let salt = STANDARD.decode(&keyfile.salt).map_err(|e| CredentialsError::Base64(e.to_string()))?;
    let nonce_bytes = STANDARD.decode(&keyfile.nonce).map_err(|e| CredentialsError::Base64(e.to_string()))?;
    let ciphertext = STANDARD.decode(&keyfile.ciphertext).map_err(|e| CredentialsError::Base64(e.to_string()))?;

    let key = derive_key(passphrase, &salt, &keyfile.kdf_params)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_ref()));
    let nonce = Nonce::from_slice(&nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| CredentialsError::DecryptionFailed)?;

    Ok(Zeroizing::new(plaintext))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass(s: &str) -> SecretString {
        SecretString::new(s.to_string())
    }

    #[test]
    fn encrypt_then_decrypt_round_trips() {
        let secret_bytes = b"api_key=ABCD&api_secret=EFGH".to_vec();
        let kf = encrypt_payload(&pass("correct horse battery staple"), &secret_bytes).unwrap();
        let decrypted = decrypt_payload(&pass("correct horse battery staple"), &kf).unwrap();
        assert_eq!(decrypted.as_slice(), secret_bytes.as_slice());
    }

    #[test]
    fn wrong_passphrase_fails_cleanly() {
        let secret_bytes = b"some kraken credentials payload".to_vec();
        let kf = encrypt_payload(&pass("right passphrase"), &secret_bytes).unwrap();
        let result = decrypt_payload(&pass("wrong passphrase"), &kf);
        assert!(matches!(result, Err(CredentialsError::DecryptionFailed)));
    }

    #[test]
    fn corrupted_ciphertext_fails_cleanly() {
        let secret_bytes = b"another payload".to_vec();
        let mut kf = encrypt_payload(&pass("passphrase"), &secret_bytes).unwrap();
        // Flip a byte in the stored ciphertext to simulate corruption.
        let mut raw = STANDARD.decode(&kf.ciphertext).unwrap();
        raw[0] ^= 0xFF;
        kf.ciphertext = STANDARD.encode(raw);

        let result = decrypt_payload(&pass("passphrase"), &kf);
        assert!(matches!(result, Err(CredentialsError::DecryptionFailed)));
    }

    #[test]
    fn each_encryption_uses_a_fresh_salt_and_nonce() {
        let secret_bytes = b"payload".to_vec();
        let kf1 = encrypt_payload(&pass("same passphrase"), &secret_bytes).unwrap();
        let kf2 = encrypt_payload(&pass("same passphrase"), &secret_bytes).unwrap();
        assert_ne!(kf1.salt, kf2.salt);
        assert_ne!(kf1.nonce, kf2.nonce);
        assert_ne!(kf1.ciphertext, kf2.ciphertext);
    }
}
