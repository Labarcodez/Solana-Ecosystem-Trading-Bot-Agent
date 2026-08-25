//! `market_data`: real-time price streaming via Kraken's public WebSocket
//! v2 `ticker` feed (`kraken_ws.rs`), plus a CSV replay source (`mock.rs`)
//! that powers `--price-source mock` for a zero-cost, zero-API-key
//! end-to-end run.

pub mod error;
pub mod kraken_ws;
pub mod mock;

pub use error::MarketDataError;
pub use kraken_ws::{stream_prices, DEFAULT_WS_URL};
