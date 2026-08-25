//! Terminal passphrase prompting. Uses `rpassword` so the passphrase is
//! never echoed to the terminal and never appears in shell history or logs.

use secrecy::{ExposeSecret, SecretString};

use crate::error::WalletError;

pub fn prompt_passphrase(prompt: &str) -> Result<SecretString, WalletError> {
    let raw = rpassword::prompt_password(prompt)?;
    Ok(SecretString::new(raw))
}

/// Prompts twice and loops until both entries match - used by `wallet init`
/// so a typo doesn't lock the user out of a wallet they just created.
pub fn prompt_new_passphrase() -> Result<SecretString, WalletError> {
    loop {
        let first = prompt_passphrase("New wallet passphrase: ")?;
        let second = prompt_passphrase("Confirm passphrase: ")?;
        if first.expose_secret() == second.expose_secret() {
            return Ok(first);
        }
        eprintln!("Passphrases did not match - try again.");
    }
}
