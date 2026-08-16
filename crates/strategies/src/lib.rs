//! Concrete `Strategy` implementations, plus the [`build_strategy`] factory
//! that both `bin/trading-bot` and `crates/backtester` call to construct a
//! strategy by name. Using one shared factory - rather than each binary
//! instantiating strategies itself - is what guarantees live trading and
//! backtesting run byte-for-byte the same strategy logic.

pub mod grid;
pub mod momentum;

pub use grid::{GridConfig, GridStrategy};
pub use momentum::{MomentumConfig, MomentumStrategy};

use bot_core::Strategy;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StrategyError {
    #[error("unknown strategy \"{0}\" (expected \"momentum\" or \"grid\")")]
    Unknown(String),
}

/// Build a strategy by name. Both `momentum` and `grid` configs are always
/// passed in (mirroring `config.toml` always having both `[strategy.momentum]`
/// and `[strategy.grid]` sections) - only the one matching `name` is used.
pub fn build_strategy(
    name: &str,
    momentum: &MomentumConfig,
    grid: &GridConfig,
) -> Result<Box<dyn Strategy>, StrategyError> {
    match name {
        "momentum" => Ok(Box::new(MomentumStrategy::new(momentum.clone()))),
        "grid" => Ok(Box::new(GridStrategy::new(grid.clone()))),
        other => Err(StrategyError::Unknown(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_known_strategies() {
        let m = MomentumConfig::default();
        let g = GridConfig::default();
        assert_eq!(build_strategy("momentum", &m, &g).unwrap().name(), "momentum");
        assert_eq!(build_strategy("grid", &m, &g).unwrap().name(), "grid");
    }

    #[test]
    fn rejects_unknown_strategy() {
        let m = MomentumConfig::default();
        let g = GridConfig::default();
        assert!(build_strategy("scalping", &m, &g).is_err());
    }
}
