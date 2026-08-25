//! Mirrors `config.toml` exactly. Split into per-table structs the same
//! way `bin/backtest` does, since both binaries read the same file and
//! this keeps the mapping obvious against `config/config.toml`.

use std::collections::HashMap;

use bot_core::{MarketType, RiskTier};
use risk::{RiskConfig, TierConfig};
use serde::Deserialize;
use strategies::{FundingCarryConfig, GridConfig, MarketMakerConfig, MomentumConfig, TriangularArbitrageConfig};

#[derive(Debug, Clone, Deserialize)]
pub struct GeneralSection {
    pub mode: String,
    pub strategy: String,
    /// The quote currency this run's paper capital / live-balance lookup
    /// is denominated in (e.g. `"USD"`) - used to pick a balance entry out
    /// of Kraken's `Balance` response in live mode.
    pub base_currency: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StrategySection {
    pub momentum: MomentumConfig,
    pub grid: GridConfig,
    pub market_maker: MarketMakerConfig,
    pub triangular_arbitrage: TriangularArbitrageConfig,
    pub funding_carry: FundingCarryConfig,
}

/// Mirrors `[risk]` including its nested `[risk.tiers.*]` sub-tables.
#[derive(Debug, Clone, Deserialize)]
pub struct RiskSection {
    pub max_open_positions: usize,
    pub daily_loss_limit_quote: f64,
    pub daily_loss_limit_pct: f64,
    pub default_stop_loss_pct: f64,
    pub default_take_profit_pct: f64,
    pub tiers: HashMap<String, TierConfig>,
}

impl RiskSection {
    pub fn into_parts(self) -> (RiskConfig, HashMap<RiskTier, TierConfig>) {
        let risk_cfg = RiskConfig {
            max_open_positions: self.max_open_positions,
            daily_loss_limit_quote: self.daily_loss_limit_quote,
            daily_loss_limit_pct: self.daily_loss_limit_pct,
            default_stop_loss_pct: self.default_stop_loss_pct,
            default_take_profit_pct: self.default_take_profit_pct,
        };
        let mut tiers = HashMap::new();
        for (name, cfg) in self.tiers {
            let tier = match name.as_str() {
                "spot" => RiskTier::Spot,
                "margin" => RiskTier::Margin,
                "futures" => RiskTier::Futures,
                other => {
                    eprintln!("warning: ignoring unknown [risk.tiers.{other}] section in config");
                    continue;
                }
            };
            tiers.insert(tier, cfg);
        }
        (risk_cfg, tiers)
    }
}

/// Mirrors `[kraken]`: the static, config-driven tradable set (no
/// discovery/safety pipeline in this build - see README for why that's a
/// deliberate scope decision, not a gap).
#[derive(Debug, Clone, Deserialize)]
pub struct KrakenSection {
    /// Spot/margin pairs to stream live prices for via Kraken's public
    /// WebSocket v2 `ticker` channel, e.g. `"XBT/USD"`. Confirm the exact
    /// symbol spelling your account's calls expect via Kraken's own
    /// `AssetPairs` endpoint - see `execution::executor`'s module docs.
    #[serde(default)]
    pub pairs: Vec<String>,
    /// Which product `pairs` trade as. `Margin` rides the same
    /// Spot API with a `leverage` parameter; `Futures` pairs belong in
    /// `futures_pairs` instead, not here.
    #[serde(default)]
    pub market_type: MarketType,
    /// Leverage requested for `Margin`/`Futures` positions. Ignored for `Spot`.
    #[serde(default = "default_leverage")]
    pub leverage: f64,
    /// Kraken Futures perpetual symbols to poll (e.g. `"PI_XBTUSD"`) -
    /// powers `PriceTick::funding_rate` for `FundingCarryStrategy`. Polled
    /// over REST on an interval (no Futures WebSocket client in this
    /// build - see README).
    #[serde(default)]
    pub futures_pairs: Vec<String>,
    #[serde(default = "default_futures_poll_secs")]
    pub futures_poll_secs: u64,
}

fn default_leverage() -> f64 {
    1.0
}

fn default_futures_poll_secs() -> u64 {
    30
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutionSection {
    pub slippage_bps: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub general: GeneralSection,
    pub strategy: StrategySection,
    pub risk: RiskSection,
    pub kraken: KrakenSection,
    pub execution: ExecutionSection,
    /// Not consumed by `main.rs` - dry-run/mock uses `--starting-capital-quote`
    /// and live mode fetches the account's real Kraken balance instead. Kept
    /// for config-file completeness so `bin/backtest` and `bin/trading-bot`
    /// keep reading the exact same `config.toml`.
    #[allow(dead_code)]
    pub backtest: backtester::BacktestConfig,
}
