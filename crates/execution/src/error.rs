use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Kraken Spot API error: {0}")]
    KrakenSpot(String),

    #[error("Kraken Futures API error: {0}")]
    KrakenFutures(String),

    #[error("unexpected response shape: {0}")]
    UnexpectedResponse(String),

    #[error("base64 decode error: {0}")]
    Base64(String),

    #[error("live execution requires credentials for this market type, but none were supplied")]
    MissingCredentials,

    #[error("no ticker data returned for pair {0}")]
    NoTickerData(String),
}
