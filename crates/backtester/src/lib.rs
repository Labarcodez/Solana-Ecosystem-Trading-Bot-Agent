//! `backtester`: replays historical price data through the same
//! `Strategy`/`RiskManager` code the live bot uses, filling orders against a
//! modeled fee+slippage curve instead of a real DEX. See `engine.rs` for
//! the honesty notes on what this does and doesn't model.

pub mod csv_loader;
pub mod engine;
pub mod error;
pub mod report;
pub mod simulated_executor;

pub use engine::{run, BacktestConfig, BacktestParams};
pub use error::BacktestError;
pub use report::BacktestReport;
pub use simulated_executor::{SimulatedExecutor, SimulatedExecutorConfig};
