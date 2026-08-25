//! Terminal secret prompting. Uses `rpassword` so nothing typed here is ever
//! echoed to the terminal or appears in shell history or logs - used both
//! for the encryption passphrase and for the Kraken API key/secret
//! themselves during `credentials init`.

use secrecy::SecretString;

use crate::error::CredentialsError;

pub fn prompt_passphrase(prompt: &str) -> Result<SecretString, CredentialsError> {
    let raw = rpassword::prompt_password(prompt)?;
    Ok(SecretString::new(raw))
}

/// Prompts twice and loops until both entries match - used by
/// `credentials init` so a typo doesn't lock the user out of a file they
/// just created.
pub fn prompt_new_passphrase() -> Result<SecretString, CredentialsError> {
    use secrecy::ExposeSecret;
    loop {
        let first = prompt_passphrase("New encryption passphrase: ")?;
        let second = prompt_passphrase("Confirm passphrase: ")?;
        if first.expose_secret() == second.expose_secret() {
            return Ok(first);
        }
        eprintln!("Passphrases did not match - try again.");
    }
}

/// Prompts once for a single secret value (the Kraken API key or API
/// secret), never echoed.
pub fn prompt_secret(label: &str) -> Result<String, CredentialsError> {
    Ok(rpassword::prompt_password(label)?)
}
