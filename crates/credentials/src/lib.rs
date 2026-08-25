//! `credentials`: encrypted-at-rest Kraken API key/secret pair. The
//! plaintext credentials exist in memory only after the user types the
//! correct passphrase, and only for as long as the process needs them - see
//! `crypto.rs` for the Argon2id + AES-256-GCM implementation.
//!
//! Unlike the Solana wallet this replaces, there is no on-chain signing
//! concept at all here: a Kraken API key/secret pair is just two opaque
//! strings used to HMAC-sign HTTP requests (see `execution::kraken_spot`/
//! `kraken_futures`). Both fields are held in [`zeroize::Zeroizing`]
//! wrappers, so - unlike the old `solana_sdk::Keypair`, whose internal
//! storage wasn't zeroize-aware - this crate can genuinely guarantee the
//! plaintext is wiped when a `KrakenCredentials` value is dropped.
//!
//! Kraken recommends a separate API key for its Futures product from the
//! one used for Spot/Margin; this crate is deliberately generic over *which*
//! credentials it's protecting (see `bin/trading-bot`'s
//! `KRAKEN_CREDENTIALS_PATH`/`KRAKEN_FUTURES_CREDENTIALS_PATH` env vars) -
//! it just encrypts/decrypts one key/secret pair per file.

pub mod crypto;
pub mod error;
pub mod keyfile;
pub mod prompt;

pub use error::CredentialsError;
pub use keyfile::KeyFile;

use std::path::Path;

use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// A decrypted Kraken API key/secret pair, held only as long as the caller
/// needs it. Both fields zeroize their backing memory on drop.
pub struct KrakenCredentials {
    pub api_key: Zeroizing<String>,
    pub api_secret: Zeroizing<String>,
}

/// The plaintext shape encrypted inside the key file - kept private and
/// separate from `KrakenCredentials` so the wire/at-rest format doesn't
/// need to carry the `Zeroizing` wrapper (serde doesn't need it; the
/// deserialized `String`s are copied into `Zeroizing` fields immediately
/// after decoding, and the intermediate JSON `String`s are dropped without
/// a durable reference once that copy happens).
#[derive(Serialize, Deserialize)]
struct CredentialsPayload {
    api_key: String,
    api_secret: String,
}

/// Encrypt `api_key`/`api_secret` under `passphrase` and write them to
/// `path`. Refuses to overwrite an existing file - callers wanting to
/// re-key an existing credentials file must move/delete the old file first.
pub fn init_new(
    path: &Path,
    passphrase: &SecretString,
    api_key: &str,
    api_secret: &str,
) -> Result<(), CredentialsError> {
    if api_key.trim().is_empty() || api_secret.trim().is_empty() {
        return Err(CredentialsError::EmptyCredential);
    }
    let payload = CredentialsPayload { api_key: api_key.to_string(), api_secret: api_secret.to_string() };
    let json = serde_json::to_vec(&payload)?;
    let kf = crypto::encrypt_payload(passphrase, &json)?;
    kf.save(path)?;
    Ok(())
}

/// Load and decrypt the credentials at `path`.
pub fn unlock(path: &Path, passphrase: &SecretString) -> Result<KrakenCredentials, CredentialsError> {
    let kf = KeyFile::load(path)?;
    let bytes = crypto::decrypt_payload(passphrase, &kf)?;
    let payload: CredentialsPayload = serde_json::from_slice(&bytes)?;
    Ok(KrakenCredentials {
        api_key: Zeroizing::new(payload.api_key),
        api_secret: Zeroizing::new(payload.api_secret),
    })
}

/// Interactive `credentials init` CLI flow: prompts for the Kraken API key
/// and API secret (both hidden, never echoed), then twice for a new
/// encryption passphrase, and writes the encrypted file.
pub fn init_interactive(path: &Path) -> Result<(), CredentialsError> {
    let api_key = prompt::prompt_secret("Kraken API key: ")?;
    let api_secret = prompt::prompt_secret("Kraken API secret: ")?;
    let passphrase = prompt::prompt_new_passphrase()?;
    init_new(path, &passphrase, &api_key, &api_secret)
}

/// Interactive startup flow: prompts once, with a bounded retry loop on a
/// wrong passphrase, and returns the unlocked credentials.
pub fn unlock_interactive(path: &Path) -> Result<KrakenCredentials, CredentialsError> {
    const MAX_ATTEMPTS: u32 = 3;
    for attempt in 1..=MAX_ATTEMPTS {
        let passphrase = prompt::prompt_passphrase("Credentials passphrase: ")?;
        match unlock(path, &passphrase) {
            Ok(creds) => return Ok(creds),
            Err(CredentialsError::DecryptionFailed) if attempt < MAX_ATTEMPTS => {
                eprintln!("Wrong passphrase, try again ({attempt}/{MAX_ATTEMPTS}).");
            }
            Err(e) => return Err(e),
        }
    }
    Err(CredentialsError::DecryptionFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile_shim::temp_path;

    // Minimal temp-file helper so this crate doesn't need a `tempfile` dev-dependency.
    mod tempfile_shim {
        use std::path::PathBuf;

        pub fn temp_path(name: &str) -> PathBuf {
            let mut p = std::env::temp_dir();
            p.push(format!("credentials_test_{}_{}", std::process::id(), name));
            p
        }
    }

    fn pass(s: &str) -> SecretString {
        SecretString::new(s.to_string())
    }

    #[test]
    fn init_new_then_unlock_round_trips_the_same_credentials() {
        let path = temp_path("init_unlock.enc.json");
        let _ = std::fs::remove_file(&path);

        init_new(&path, &pass("test passphrase"), "my-api-key", "my-api-secret").unwrap();
        let unlocked = unlock(&path, &pass("test passphrase")).unwrap();
        assert_eq!(unlocked.api_key.as_str(), "my-api-key");
        assert_eq!(unlocked.api_secret.as_str(), "my-api-secret");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn unlock_with_wrong_passphrase_fails_cleanly() {
        let path = temp_path("wrong_pass.enc.json");
        let _ = std::fs::remove_file(&path);

        init_new(&path, &pass("right"), "key", "secret").unwrap();
        let result = unlock(&path, &pass("wrong"));
        assert!(matches!(result, Err(CredentialsError::DecryptionFailed)));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn init_new_refuses_to_overwrite_existing_file() {
        let path = temp_path("no_overwrite.enc.json");
        let _ = std::fs::remove_file(&path);

        init_new(&path, &pass("first"), "key1", "secret1").unwrap();
        let second = init_new(&path, &pass("second"), "key2", "secret2");
        assert!(matches!(second, Err(CredentialsError::AlreadyExists(_))));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn init_new_rejects_an_empty_api_key_or_secret() {
        let path = temp_path("empty_cred.enc.json");
        let _ = std::fs::remove_file(&path);

        let result = init_new(&path, &pass("pw"), "", "secret");
        assert!(matches!(result, Err(CredentialsError::EmptyCredential)));
        assert!(!path.exists(), "must not write a file for a rejected credential");
    }
}
