//! Replays historical prices through the exact same `Strategy` and
//! `RiskManager` code the live bot uses (built via the same
//! `strategies::build_strategy` factory) - see the README for why that
//! equivalence is the whole point of this crate.

use std::collections::HashMap;
use std::path::Path;

use bot_core::{AppEvent, Fill, Side, Strategy, TokenMeta, TokenPhase, TrustTier};
use risk::{RiskConfig, RiskDecision, RiskManager, TierConfig};
use serde::{Deserialize, Serialize};
use strategies::{build_strategy, GridConfig, MomentumConfig};

use crate::csv_loader::load_price_csv;
use crate::error::BacktestError;
use crate::report::BacktestReport;
use crate::simulated_executor::{SimulatedExecutor, SimulatedExecutorConfig};

/// Mirrors `config.toml`'s `[backtest]` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestConfig {
    pub data_file: String,
    pub slippage_bps: u16,
    pub fee_bps: u16,
    pub starting_capital_sol: f64,
    #[serde(default)]
    pub jito_tip_lamports: u64,
}

pub struct BacktestParams<'a> {
    pub strategy_name: &'a str,
    pub momentum: &'a MomentumConfig,
    pub grid: &'a GridConfig,
    pub risk: &'a RiskConfig,
    pub tiers: &'a HashMap<TrustTier, TierConfig>,
    pub backtest: &'a BacktestConfig,
    pub data_path: &'a Path,
}

pub fn run(params: &BacktestParams) -> Result<BacktestReport, BacktestError> {
    let mint = bot_core::Pubkey::new_unique(); // single synthetic asset id for the whole run
    let ticks = load_price_csv(params.data_path, mint)?;

    let mut strategy = build_strategy(params.strategy_name, params.momentum, params.grid)?;
    let mut risk_mgr = RiskManager::new(
        params.risk.clone(),
        params.tiers.clone(),
        params.backtest.slippage_bps,
        params.backtest.starting_capital_sol,
    );
    let executor = SimulatedExecutor::new(SimulatedExecutorConfig {
        slippage_bps: params.backtest.slippage_bps,
        fee_bps: params.backtest.fee_bps,
        jito_tip_lamports: params.backtest.jito_tip_lamports,
    });

    let token_meta = TokenMeta {
        mint,
        phase: TokenPhase::Migrated,
        trust_tier: TrustTier::Established,
        discovered_at: ticks[0].ts,
        source: "backtest".into(),
    };

    let mut equity_curve = Vec::with_capacity(ticks.len());
    let mut trade_pnls = Vec::new();
    let mut last_entry_price: Option<f64> = None;
    let mut breaker_trips: u32 = 0;

    for tick in &ticks {
        // Protective exits (stop-loss/take-profit) fire on every tick
        // regardless of whether the strategy emits anything this tick.
        let protective_orders = risk_mgr.on_price_tick(tick);
        for order in protective_orders {
            let fill = executor.fill(&order, tick.price);
            apply_fill(&fill, &mut risk_mgr, strategy.as_mut(), &mut last_entry_price, &mut trade_pnls);
        }

        // Strategy-driven signals, gated by risk - the only path a trade can take.
        let signals = strategy.on_price_tick(tick);
        for signal in signals {
            if let RiskDecision::Approved(order) = risk_mgr.evaluate_signal(&signal, &token_meta, tick.price) {
                let fill = executor.fill(&order, tick.price);
                apply_fill(&fill, &mut risk_mgr, strategy.as_mut(), &mut last_entry_price, &mut trade_pnls);
            }
        }

        for event in risk_mgr.drain_events() {
            if matches!(event, AppEvent::CircuitBreakerTripped { .. }) {
                breaker_trips += 1;
            }
        }

        equity_curve.push((tick.ts, risk_mgr.equity_sol()));
    }

    Ok(BacktestReport::compute(equity_curve, &trade_pnls, breaker_trips))
}

fn apply_fill(
    fill: &Fill,
    risk_mgr: &mut RiskManager,
    strategy: &mut dyn Strategy,
    last_entry_price: &mut Option<f64>,
    trade_pnls: &mut Vec<f64>,
) {
    match fill.side {
        Side::Buy => {
            *last_entry_price = Some(fill.price);
        }
        Side::Sell => {
            if let Some(entry) = last_entry_price.take() {
                let pnl = (fill.price - entry) * fill.qty - fill.fee_sol - fill.jito_tip_sol;
                trade_pnls.push(pnl);
            }
        }
    }
    risk_mgr.on_fill(fill);
    strategy.on_fill(fill);
}

#[cfg(test)]
mod tests {
    use super::*;
    use risk::default_tiers;

    fn synthetic_csv(path: &Path, prices: &[f64]) {
        let mut content = String::from("timestamp,price\n");
        for (i, p) in prices.iter().enumerate() {
            content.push_str(&format!("{},{p}\n", i as i64 * 60));
        }
        std::fs::write(path, content).unwrap();
    }

    fn base_backtest_cfg() -> BacktestConfig {
        BacktestConfig {
            data_file: String::new(),
            slippage_bps: 50,
            fee_bps: 30,
            starting_capital_sol: 100.0,
            jito_tip_lamports: 5_000,
        }
    }

    #[test]
    fn runs_end_to_end_and_produces_a_report() {
        let path = std::env::temp_dir().join("bt_engine_e2e.csv");
        // A clear uptrend followed by a pullback should give momentum
        // something real to trade on.
        let mut prices = vec![100.0; 25];
        for i in 0..20 {
            prices.push(100.0 + i as f64 * 2.0);
        }
        for i in 0..10 {
            prices.push(140.0 - i as f64 * 3.0);
        }
        synthetic_csv(&path, &prices);

        let momentum = MomentumConfig::default();
        let grid = GridConfig::default();
        let risk_cfg = RiskConfig::default();
        let tiers = default_tiers();
        let bt_cfg = base_backtest_cfg();

        let params = BacktestParams {
            strategy_name: "momentum",
            momentum: &momentum,
            grid: &grid,
            risk: &risk_cfg,
            tiers: &tiers,
            backtest: &bt_cfg,
            data_path: &path,
        };
        let report = run(&params).unwrap();
        assert_eq!(report.equity_curve.len(), prices.len());
        assert!(report.starting_equity_sol > 0.0);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn tighter_threshold_changes_the_report_not_just_hardcoded_numbers() {
        // The acceptance check called out in the plan: changing a strategy
        // parameter must measurably change the backtest output.
        let path = std::env::temp_dir().join("bt_engine_param_sensitivity.csv");
        let mut prices = vec![100.0; 25];
        for i in 0..30 {
            prices.push(100.0 + (i as f64 * 0.7).sin() * 5.0 + i as f64 * 0.5);
        }
        synthetic_csv(&path, &prices);

        let grid = GridConfig::default();
        let risk_cfg = RiskConfig::default();
        let tiers = default_tiers();
        let bt_cfg = base_backtest_cfg();

        let loose = MomentumConfig { threshold_pct: 0.05, cooldown_secs: 0, ..MomentumConfig::default() };
        let strict = MomentumConfig { threshold_pct: 20.0, cooldown_secs: 0, ..MomentumConfig::default() };

        let report_loose = run(&BacktestParams {
            strategy_name: "momentum", momentum: &loose, grid: &grid,
            risk: &risk_cfg, tiers: &tiers, backtest: &bt_cfg, data_path: &path,
        }).unwrap();
        let report_strict = run(&BacktestParams {
            strategy_name: "momentum", momentum: &strict, grid: &grid,
            risk: &risk_cfg, tiers: &tiers, backtest: &bt_cfg, data_path: &path,
        }).unwrap();

        // A near-zero threshold should trade readily; an all-but-impossible
        // one (20% SMA separation) should barely trade at all. Compare the
        // resulting equity path rather than just closed-trade count, since
        // a position opened-but-not-yet-closed by data's end already moves
        // equity via mark-to-market and is itself evidence the parameter
        // changed behavior, not just the final report numbers.
        assert_ne!(
            report_loose.equity_curve, report_strict.equity_curve,
            "different thresholds over the same price path must produce a different equity path"
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn unknown_strategy_name_is_a_clean_error() {
        let path = std::env::temp_dir().join("bt_engine_unknown_strategy.csv");
        synthetic_csv(&path, &[100.0, 101.0, 99.0]);

        let momentum = MomentumConfig::default();
        let grid = GridConfig::default();
        let risk_cfg = RiskConfig::default();
        let tiers = default_tiers();
        let bt_cfg = base_backtest_cfg();

        let result = run(&BacktestParams {
            strategy_name: "not_a_real_strategy", momentum: &momentum, grid: &grid,
            risk: &risk_cfg, tiers: &tiers, backtest: &bt_cfg, data_path: &path,
        });
        assert!(result.is_err());

        std::fs::remove_file(&path).ok();
    }
}
