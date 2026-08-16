//! `market_data`: real-time price streaming via Shyft's Yellowstone gRPC
//! feed (`yellowstone.rs`), decoding generic AMM vault balances into prices
//! (`pool_price.rs`), plus a CSV replay source (`mock.rs`) that powers
//! `--price-source mock` for a zero-cost, zero-API-key end-to-end run.

pub mod error;
pub mod mock;
pub mod pool_price;
pub mod yellowstone;

pub use error::MarketDataError;
pub use yellowstone::{stream_prices, WatchedPool, YellowstoneConfig};
