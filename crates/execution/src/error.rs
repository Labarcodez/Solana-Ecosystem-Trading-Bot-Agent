use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Jupiter API error: {0}")]
    Jupiter(String),

    #[error("Jito API error: {0}")]
    Jito(String),

    #[error("unexpected response shape: {0}")]
    UnexpectedResponse(String),

    #[error("base64 decode error: {0}")]
    Base64(String),

    #[error("invalid pubkey: {0}")]
    InvalidPubkey(String),
}
