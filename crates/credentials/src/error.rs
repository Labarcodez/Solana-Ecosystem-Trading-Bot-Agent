use thiserror::Error;

#[derive(Debug, Error)]
pub enum WalletError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid base64 in key file: {0}")]
    Base64(String),

    #[error("wrong passphrase or corrupted key file")]
    DecryptionFailed,

    #[error("key derivation failed: {0}")]
    Kdf(String),

    #[error("unsupported key file version {0}")]
    UnsupportedVersion(u8),

    #[error("unsupported kdf \"{0}\" (expected \"argon2id\")")]
    UnsupportedKdf(String),

    #[error("unsupported cipher \"{0}\" (expected \"aes-256-gcm\")")]
    UnsupportedCipher(String),

    #[error("decrypted key has wrong length: expected {expected}, got {got}")]
    BadKeyLength { expected: usize, got: usize },

    #[error("invalid keypair bytes: {0}")]
    InvalidKeypair(String),

    #[error("passphrases did not match")]
    PassphraseMismatch,

    #[error("key file already exists at {0} - refusing to overwrite")]
    AlreadyExists(String),
}
