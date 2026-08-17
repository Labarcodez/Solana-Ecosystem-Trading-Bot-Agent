use thiserror::Error;

#[derive(Debug, Error)]
pub enum MarketDataError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("csv error: {0}")]
    Csv(#[from] csv::Error),

    #[error("gRPC connect/subscribe error: {0}")]
    Grpc(String),

    #[error("account data too short to be a valid SPL token account: {len} bytes")]
    ShortAccountData { len: usize },

    #[error("invalid pubkey bytes in gRPC update ({len} bytes, expected 32)")]
    InvalidPubkeyBytes { len: usize },

    #[error("vault balance is zero, cannot compute price")]
    ZeroVaultBalance,

    #[error("account data does not match the expected discriminator/type")]
    WrongAccountType,
}
