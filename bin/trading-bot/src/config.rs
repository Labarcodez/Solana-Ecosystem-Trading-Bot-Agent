//! Mirrors `config.toml` exactly. Split into per-table structs the same
//! way `bin/backtest` does, since both binaries read the same file and
//! this keeps the mapping obvious against `config/config.toml`.

use std::collections::HashMap;

use bot_core::TrustTier;
use risk::{RiskConfig, TierConfig};
use serde::Deserialize;
use strategies::{GridConfig, MomentumConfig};

#[derive(Debug, Clone, Deserialize)]
pub struct GeneralSection {
    pub mode: String,
    pub strategy: String,
    #[allow(dead_code)]
    pub base_currency: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StrategySection {
    pub momentum: MomentumConfig,
    pub grid: GridConfig,
}

/// Mirrors `[risk]` including its nested `[risk.tiers.*]` sub-tables.
#[derive(Debug, Clone, Deserialize)]
pub struct RiskSection {
    pub max_open_positions: usize,
    pub daily_loss_limit_sol: f64,
    pub daily_loss_limit_pct: f64,
    pub default_stop_loss_pct: f64,
    pub default_take_profit_pct: f64,
    pub tiers: HashMap<String, TierConfig>,
}

impl RiskSection {
    pub fn into_parts(self) -> (RiskConfig, HashMap<TrustTier, TierConfig>) {
        let risk_cfg = RiskConfig {
            max_open_positions: self.max_open_positions,
            daily_loss_limit_sol: self.daily_loss_limit_sol,
            daily_loss_limit_pct: self.daily_loss_limit_pct,
            default_stop_loss_pct: self.default_stop_loss_pct,
            default_take_profit_pct: self.default_take_profit_pct,
        };
        let mut tiers = HashMap::new();
        for (name, cfg) in self.tiers {
            let tier = match name.as_str() {
                "bonding_curve" => TrustTier::BondingCurve,
                "migrated_new" => TrustTier::MigratedNew,
                "established" => TrustTier::Established,
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

/// Read in full (matching `config.toml`) even though `main.rs` doesn't
/// consume every field yet - `--price-source live` in this build only logs
/// a "needs your own credentials" warning rather than driving a live
/// discovery/safety pipeline (see README for what's wired vs. documented as
/// a follow-up). Kept as a complete mirror of the config file now so wiring
/// the live path later doesn't require touching this struct.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct DiscoverySection {
    pub enabled: bool,
    pub watch_pumpfun_bonding_curve: bool,
    pub watch_pumpswap: bool,
    pub watch_raydium: bool,
    pub watch_orca: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutionSection {
    pub slippage_bps: u16,
    #[allow(dead_code)]
    pub priority_fee_lamports: u64,
    #[allow(dead_code)]
    pub jito_tip_lamports: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct BacktestSection {
    pub data_file: String,
    pub slippage_bps: u16,
    pub fee_bps: u16,
    pub starting_capital_sol: f64,
    #[serde(default)]
    pub jito_tip_lamports: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct AppConfig {
    pub general: GeneralSection,
    pub strategy: StrategySection,
    pub risk: RiskSection,
    pub discovery: DiscoverySection,
    pub safety: safety::SafetyConfig,
    pub execution: ExecutionSection,
    /// Not consumed by `main.rs` in this build - dry-run/mock uses
    /// `--starting-capital-sol` and live mode fetches the wallet's real
    /// on-chain balance instead. Kept for config-file completeness and for
    /// a future live wiring pass to reuse.
    pub backtest: BacktestSection,
}
