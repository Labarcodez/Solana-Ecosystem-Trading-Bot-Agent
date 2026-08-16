//! `bot-core`: domain types and the `Strategy` trait shared by every crate in
//! the workspace. No async runtime, no network clients - kept that way
//! deliberately so `strategies`, `risk`, and `backtester` are trivially
//! unit-testable and so `bin/trading-bot` and `bin/backtest` can build
//! strategies through the exact same factory function.

pub mod error;
pub mod strategy;
pub mod types;

pub use error::CoreError;
pub use strategy::Strategy;
pub use types::*;
