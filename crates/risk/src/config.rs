use std::collections::HashMap;

use bot_core::RiskTier;
use serde::{Deserialize, Serialize};

/// Mirrors `config.toml`'s `[risk]` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    pub max_open_positions: usize,
    pub daily_loss_limit_quote: f64,
    pub daily_loss_limit_pct: f64,
    pub default_stop_loss_pct: f64,
    pub default_take_profit_pct: f64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_open_positions: 5,
            daily_loss_limit_quote: 50.0,
            daily_loss_limit_pct: 10.0,
            default_stop_loss_pct: 8.0,
            default_take_profit_pct: 15.0,
        }
    }
}

/// Mirrors one `[risk.tiers.*]` table. Sizing, leverage, and mandatory
/// safety-floor rules are keyed by `RiskTier` (spot/margin/futures), not
/// global - the more leverage a position can carry, the tighter its
/// defaults, mirroring how the old Solana build tightened defaults for
/// less-known tokens.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TierConfig {
    pub max_position_quote: f64,
    pub risk_pct_of_capital: f64,
    /// 0.0 means "use `RiskConfig::default_stop_loss_pct` instead".
    pub mandatory_stop_loss_pct: f64,
    pub allow_take_profit_override: bool,
    /// Hard cap on requested leverage for this tier. `1.0` for `Spot`
    /// (no leverage possible there anyway).
    pub max_leverage: f64,
    /// Mandatory floor, in percent, on a margin/futures position's
    /// approximate distance-to-liquidation (see
    /// `risk_manager::approx_liquidation_distance_pct`) - a requested
    /// leverage that would put liquidation closer than this is rejected.
    /// `0.0` for `Spot` (not applicable - spot can't be liquidated).
    pub min_liquidation_distance_pct: f64,
}

pub fn default_tiers() -> HashMap<RiskTier, TierConfig> {
    let mut m = HashMap::new();
    m.insert(
        RiskTier::Spot,
        TierConfig {
            max_position_quote: 500.0,
            risk_pct_of_capital: 5.0,
            mandatory_stop_loss_pct: 0.0,
            allow_take_profit_override: true,
            max_leverage: 1.0,
            min_liquidation_distance_pct: 0.0,
        },
    );
    m.insert(
        RiskTier::Margin,
        TierConfig {
            max_position_quote: 150.0,
            risk_pct_of_capital: 2.0,
            mandatory_stop_loss_pct: 10.0,
            allow_take_profit_override: true,
            max_leverage: 3.0,
            min_liquidation_distance_pct: 20.0,
        },
    );
    m.insert(
        RiskTier::Futures,
        TierConfig {
            max_position_quote: 100.0,
            risk_pct_of_capital: 1.0,
            mandatory_stop_loss_pct: 8.0,
            allow_take_profit_override: false,
            max_leverage: 2.0,
            min_liquidation_distance_pct: 30.0,
        },
    );
    m
}
