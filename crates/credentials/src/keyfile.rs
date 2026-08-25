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
        Ok(())
    }

    /// Overwrite an existing file unconditionally - used by tests and by
    /// explicit re-key flows, never by the default `credentials init` path.
    pub fn save_overwrite(&self, path: &Path) -> Result<(), CredentialsError> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
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
