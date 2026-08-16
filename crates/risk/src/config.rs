use std::collections::HashMap;

use bot_core::TrustTier;
use serde::{Deserialize, Serialize};

/// Mirrors `config.toml`'s `[risk]` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    pub max_open_positions: usize,
    pub daily_loss_limit_sol: f64,
    pub daily_loss_limit_pct: f64,
    pub default_stop_loss_pct: f64,
    pub default_take_profit_pct: f64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_open_positions: 5,
            daily_loss_limit_sol: 0.5,
            daily_loss_limit_pct: 10.0,
            default_stop_loss_pct: 8.0,
            default_take_profit_pct: 15.0,
        }
    }
}

/// Mirrors one `[risk.tiers.*]` table. Sizing and mandatory-stop-loss rules
/// are keyed by `TrustTier`, not global - see the README section on trust
/// tiers for why.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TierConfig {
    pub max_position_sol: f64,
    pub risk_pct_of_capital: f64,
    /// 0.0 means "use `RiskConfig::default_stop_loss_pct` instead".
    pub mandatory_stop_loss_pct: f64,
    pub allow_take_profit_override: bool,
}

pub fn default_tiers() -> HashMap<TrustTier, TierConfig> {
    let mut m = HashMap::new();
    m.insert(
        TrustTier::BondingCurve,
        TierConfig {
            max_position_sol: 0.05,
            risk_pct_of_capital: 1.0,
            mandatory_stop_loss_pct: 15.0,
            allow_take_profit_override: false,
        },
    );
    m.insert(
        TrustTier::MigratedNew,
        TierConfig {
            max_position_sol: 0.15,
            risk_pct_of_capital: 2.0,
            mandatory_stop_loss_pct: 12.0,
            allow_take_profit_override: true,
        },
    );
    m.insert(
        TrustTier::Established,
        TierConfig {
            max_position_sol: 0.5,
            risk_pct_of_capital: 5.0,
            mandatory_stop_loss_pct: 0.0,
            allow_take_profit_override: true,
        },
    );
    m
}
