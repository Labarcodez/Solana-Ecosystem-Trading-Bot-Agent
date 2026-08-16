use thiserror::Error;

#[derive(Debug, Error)]
pub enum SafetyError {
    #[error("mint account data too short: {len} bytes (expected at least 82)")]
    ShortMintData { len: usize },

    #[error("invalid COption discriminant in mint account: {0}")]
    InvalidCOption(u32),

    #[error("rpc error: {0}")]
    Rpc(String),

    #[error("account not found: {0}")]
    AccountNotFound(String),
}
