use thiserror::Error;

/// Shared error type for cross-crate failures that don't warrant their own
/// per-crate error type. Most crates (credentials, execution, storage, ...)
/// define their own richer error enums and only reach for this one at
/// integration boundaries.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid configuration: {0}")]
    Config(String),

    #[error("invalid trading pair: {0}")]
    InvalidPair(String),
}
