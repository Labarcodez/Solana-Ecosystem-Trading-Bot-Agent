//! Concrete `Strategy` implementations, plus the [`build_strategy`] factory
//! that both `bin/trading-bot` and `crates/backtester` call to construct a
//! strategy by name. Using one shared factory - rather than each binary
//! instantiating strategies itself - is what guarantees live trading and
//! backtesting run byte-for-byte the same strategy logic.
//!
//! `momentum` and `grid` are general-purpose, venue-agnostic strategies
//! carried over from the original design. `market_maker`,
//! `triangular_arbitrage`, and `funding_carry` are new, Kraken-specific
//! additions - the concrete mechanisms behind the README's "how to make
//! the most money on Kraken" section. Each module's doc comment documents
//! exactly what real edge it captures and what it honestly does not (none
//! of them are risk-free).

pub mod funding_carry;
pub mod grid;
pub mod market_maker;
pub mod momentum;
pub mod triangular_arbitrage;

pub use funding_carry::{FundingCarryConfig, FundingCarryStrategy};
pub use grid::{GridConfig, GridStrategy};
pub use market_maker::{MarketMakerConfig, MarketMakerStrategy};
pub use momentum::{MomentumConfig, MomentumStrategy};
pub use triangular_arbitrage::{TriangularArbitrageConfig, TriangularArbitrageStrategy};

use bot_core::Strategy;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StrategyError {
    #[error("unknown strategy \"{0}\" (expected \"momentum\", \"grid\", \"market_maker\", \"triangular_arbitrage\", or \"funding_carry\")")]
    Unknown(String),
}

/// Build a strategy by name. Every strategy's config is always passed in
/// (mirroring `config.toml` always having every `[strategy.*]` section) -
/// only the one matching `name` is used.
#[allow(clippy::too_many_arguments)]
pub fn build_strategy(
    name: &str,
    momentum: &MomentumConfig,
    grid: &GridConfig,
    market_maker: &MarketMakerConfig,
    triangular_arbitrage: &TriangularArbitrageConfig,
    funding_carry: &FundingCarryConfig,
) -> Result<Box<dyn Strategy>, StrategyError> {
    match name {
        "momentum" => Ok(Box::new(MomentumStrategy::new(momentum.clone()))),
        "grid" => Ok(Box::new(GridStrategy::new(grid.clone()))),
        "market_maker" => Ok(Box::new(MarketMakerStrategy::new(market_maker.clone()))),
        "triangular_arbitrage" => Ok(Box::new(TriangularArbitrageStrategy::new(triangular_arbitrage.clone()))),
        "funding_carry" => Ok(Box::new(FundingCarryStrategy::new(funding_carry.clone()))),
        other => Err(StrategyError::Unknown(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_configs() -> (MomentumConfig, GridConfig, MarketMakerConfig, TriangularArbitrageConfig, FundingCarryConfig) {
        (
            MomentumConfig::default(),
            GridConfig::default(),
            MarketMakerConfig::default(),
            TriangularArbitrageConfig::default(),
            FundingCarryConfig::default(),
        )
    }

    #[test]
    fn builds_every_known_strategy() {
        let (m, g, mm, ta, fc) = all_configs();
        for (name, expected) in [
            ("momentum", "momentum"),
            ("grid", "grid"),
            ("market_maker", "market_maker"),
            ("triangular_arbitrage", "triangular_arbitrage"),
            ("funding_carry", "funding_carry"),
        ] {
            assert_eq!(build_strategy(name, &m, &g, &mm, &ta, &fc).unwrap().name(), expected);
        }
    }

    #[test]
    fn rejects_unknown_strategy() {
        let (m, g, mm, ta, fc) = all_configs();
        assert!(build_strategy("scalping", &m, &g, &mm, &ta, &fc).is_err());
    }
}
