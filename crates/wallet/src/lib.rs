//! `wallet`: encrypted-at-rest Solana keypair. The private key exists in
//! plaintext only in memory, only after the user types the correct
//! passphrase, and only for as long as it takes to construct a
//! `solana_sdk::signature::Keypair` from it - see `crypto.rs` for the
//! Argon2id + AES-256-GCM implementation and the honest caveat about
//! `Keypair`'s own internal storage not being zeroize-aware.

pub mod crypto;
pub mod error;
pub mod keyfile;
pub mod prompt;

pub use error::WalletError;
pub use keyfile::KeyFile;

use std::path::Path;

use secrecy::SecretString;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};
use zeroize::Zeroize;

/// Generate a brand-new keypair, encrypt it under `passphrase`, and write it
/// to `path`. Refuses to overwrite an existing file - callers wanting to
/// re-key an existing wallet must move/delete the old file first.
pub fn init_new(path: &Path, passphrase: &SecretString) -> Result<Pubkey, WalletError> {
    let keypair = Keypair::new();
    let pubkey = keypair.pubkey();
    let mut bytes = keypair.to_bytes();
    let kf = crypto::encrypt_keypair(passphrase, &bytes);
    bytes.zeroize();
    let kf = kf?;
    kf.save(path)?;
    Ok(pubkey)
}

/// Import an existing 64-byte Solana keypair and encrypt it under
/// `passphrase`. Takes ownership of `keypair_bytes` and zeroizes it before
/// returning, win or lose.
pub fn import(path: &Path, passphrase: &SecretString, mut keypair_bytes: [u8; 64]) -> Result<Pubkey, WalletError> {
    let result = (|| {
        let keypair = Keypair::try_from(&keypair_bytes[..])
            .map_err(|e| WalletError::InvalidKeypair(e.to_string()))?;
        let pubkey = keypair.pubkey();
        let kf = crypto::encrypt_keypair(passphrase, &keypair_bytes)?;
        Ok::<_, WalletError>((pubkey, kf))
    })();
    keypair_bytes.zeroize();
    let (pubkey, kf) = result?;
    kf.save(path)?;
    Ok(pubkey)
}

/// Load and decrypt the wallet at `path`, returning a live `Keypair`. The
/// intermediate decrypted byte buffer is zeroized immediately after the
/// `Keypair` is constructed from it (see the module-level caveat about
/// `Keypair`'s own internals).
pub fn unlock(path: &Path, passphrase: &SecretString) -> Result<Keypair, WalletError> {
    let kf = KeyFile::load(path)?;
    let mut bytes = crypto::decrypt_keypair(passphrase, &kf)?;
    if bytes.len() != 64 {
        bytes.zeroize();
        return Err(WalletError::BadKeyLength { expected: 64, got: bytes.len() });
    }
    let keypair = Keypair::try_from(bytes.as_slice()).map_err(|e| WalletError::InvalidKeypair(e.to_string()));
    bytes.zeroize();
    keypair
}

/// Interactive `wallet init` CLI flow: prompts twice for a new passphrase
/// (never echoed), generates a fresh keypair, and writes the encrypted key
/// file. Returns the new wallet's public key so the caller can print it.
pub fn init_interactive(path: &Path) -> Result<Pubkey, WalletError> {
    let passphrase = prompt::prompt_new_passphrase()?;
    init_new(path, &passphrase)
}

/// Interactive startup flow: prompts once, with a bounded retry loop on a
/// wrong passphrase, and returns the unlocked keypair.
pub fn unlock_interactive(path: &Path) -> Result<Keypair, WalletError> {
    const MAX_ATTEMPTS: u32 = 3;
    for attempt in 1..=MAX_ATTEMPTS {
        let passphrase = prompt::prompt_passphrase("Wallet passphrase: ")?;
        match unlock(path, &passphrase) {
            Ok(kp) => return Ok(kp),
            Err(WalletError::DecryptionFailed) if attempt < MAX_ATTEMPTS => {
                eprintln!("Wrong passphrase, try again ({attempt}/{MAX_ATTEMPTS}).");
            }
            Err(e) => return Err(e),
        }
    }
    Err(WalletError::DecryptionFailed)
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
            p.push(format!("wallet_test_{}_{}", std::process::id(), name));
            p
        }
    }

    fn pass(s: &str) -> SecretString {
        SecretString::new(s.to_string())
    }

    #[test]
    fn init_new_then_unlock_round_trips_the_same_pubkey() {
        let path = temp_path("init_unlock.enc.json");
        let _ = std::fs::remove_file(&path);

        let pubkey = init_new(&path, &pass("test passphrase")).unwrap();
        let unlocked = unlock(&path, &pass("test passphrase")).unwrap();
        assert_eq!(unlocked.pubkey(), pubkey);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn unlock_with_wrong_passphrase_fails_cleanly() {
        let path = temp_path("wrong_pass.enc.json");
        let _ = std::fs::remove_file(&path);

        init_new(&path, &pass("right")).unwrap();
        let result = unlock(&path, &pass("wrong"));
        assert!(matches!(result, Err(WalletError::DecryptionFailed)));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn init_new_refuses_to_overwrite_existing_file() {
        let path = temp_path("no_overwrite.enc.json");
        let _ = std::fs::remove_file(&path);

        init_new(&path, &pass("first")).unwrap();
        let second = init_new(&path, &pass("second"));
        assert!(matches!(second, Err(WalletError::AlreadyExists(_))));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn import_round_trips_a_known_keypair() {
        let path = temp_path("import.enc.json");
        let _ = std::fs::remove_file(&path);

        let kp = Keypair::new();
        let expected_pubkey = kp.pubkey();
        let bytes = kp.to_bytes();

        let pubkey = import(&path, &pass("import passphrase"), bytes).unwrap();
        assert_eq!(pubkey, expected_pubkey);

        let unlocked = unlock(&path, &pass("import passphrase")).unwrap();
        assert_eq!(unlocked.pubkey(), expected_pubkey);

        std::fs::remove_file(&path).ok();
    }
}
