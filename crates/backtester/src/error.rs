use thiserror::Error;

#[derive(Debug, Error)]
pub enum BacktestError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("csv error: {0}")]
    Csv(#[from] csv::Error),

    #[error("strategy error: {0}")]
    Strategy(#[from] strategies::StrategyError),

    #[error("no price data loaded from {0}")]
    EmptyDataset(String),

    #[error("invalid csv row: {0}")]
    InvalidRow(String),
}
