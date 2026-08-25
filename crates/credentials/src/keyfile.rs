//! On-disk format for the encrypted Kraken credentials file. Pure data +
//! file I/O - no cryptography here, that lives in `crypto.rs`.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::CredentialsError;

pub const CURRENT_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct KdfParams {
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

/// OWASP interactive baseline: ~19 MiB memory, 2 iterations, 1 lane. This
/// only runs once per session start, so the cost is a startup-latency
/// tradeoff, not a hot-path one.
impl Default for KdfParams {
    fn default() -> Self {
        Self { m_cost_kib: 19_456, t_cost: 2, p_cost: 1 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyFile {
    pub version: u8,
    pub kdf: String,
    pub kdf_params: KdfParams,
    /// base64-encoded, 16 random bytes
    pub salt: String,
    pub cipher: String,
    /// base64-encoded, 12-byte AES-GCM nonce
    pub nonce: String,
    /// base64-encoded AES-256-GCM ciphertext (includes the auth tag)
    pub ciphertext: String,
}

impl KeyFile {
    pub fn save(&self, path: &Path) -> Result<(), CredentialsError> {
        if path.exists() {
            return Err(CredentialsError::AlreadyExists(path.display().to_string()));
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        restrict_to_owner(path)?;
        Ok(())
    }

    /// Overwrite an existing file unconditionally - used by tests and by
    /// explicit re-key flows, never by the default `credentials init` path.
    pub fn save_overwrite(&self, path: &Path) -> Result<(), CredentialsError> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        restrict_to_owner(path)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, CredentialsError> {
        let json = fs::read_to_string(path)?;
        let kf: KeyFile = serde_json::from_str(&json)?;
        if kf.version != CURRENT_VERSION {
            return Err(CredentialsError::UnsupportedVersion(kf.version));
        }
        if kf.kdf != "argon2id" {
            return Err(CredentialsError::UnsupportedKdf(kf.kdf));
        }
        if kf.cipher != "aes-256-gcm" {
            return Err(CredentialsError::UnsupportedCipher(kf.cipher));
        }
        Ok(kf)
    }
}

/// Best-effort defense in depth: the file only ever holds AES-256-GCM
/// ciphertext, never plaintext, but there's no reason to leave it
/// group/world-readable on a multi-user machine. `fs::write` uses the
/// process umask (typically 0644 on most Linux distros); this tightens it
/// to owner-only (0600) immediately after writing. Unix-only - the same
/// notion of POSIX permission bits doesn't apply on Windows, so this is a
/// no-op there rather than a cross-platform ACL implementation.
#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> Result<(), CredentialsError> {
    use std::os::unix::fs::PermissionsExt;
    let perms = fs::Permissions::from_mode(0o600);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> Result<(), CredentialsError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> KeyFile {
        KeyFile {
            version: CURRENT_VERSION,
            kdf: "argon2id".into(),
            kdf_params: KdfParams::default(),
            salt: "c2FsdA==".into(),
            cipher: "aes-256-gcm".into(),
            nonce: "bm9uY2U=".into(),
            ciphertext: "Y2lwaGVydGV4dA==".into(),
        }
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("keyfile_test_{}_{}", std::process::id(), name));
        p
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = temp_path("round_trip.enc.json");
        let _ = fs::remove_file(&path);

        sample().save(&path).unwrap();
        let loaded = KeyFile::load(&path).unwrap();
        assert_eq!(loaded.ciphertext, sample().ciphertext);

        fs::remove_file(&path).ok();
    }

    /// A key file holds AES-256-GCM ciphertext at rest - it should never be
    /// left group/world-readable on a shared machine. See `restrict_to_owner`.
    #[cfg(unix)]
    #[test]
    fn saved_key_file_is_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let path = temp_path("perms.enc.json");
        let _ = fs::remove_file(&path);

        sample().save(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "key file must be owner-read/write only");

        fs::remove_file(&path).ok();
    }
}
