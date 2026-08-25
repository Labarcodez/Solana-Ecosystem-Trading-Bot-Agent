use thiserror::Error;

#[derive(Debug, Error)]
pub enum MarketDataError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("csv error: {0}")]
    Csv(#[from] csv::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("WebSocket connect/stream error: {0}")]
    Ws(String),
}
