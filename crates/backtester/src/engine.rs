//! Replays historical prices through the exact same `Strategy` and
//! `RiskManager` code the live bot uses (built via the same
//! `strategies::build_strategy` factory) - see the README for why that
//! equivalence is the whole point of this crate.

use std::collections::HashMap;
use std::path::Path;

use bot_core::{AppEvent, Fill, MarketType, Pair, PairMeta, RiskTier, Side, Strategy};
use risk::{RiskConfig, RiskDecision, RiskManager, TierConfig};
use serde::{Deserialize, Serialize};
use strategies::{
    build_strategy, FundingCarryConfig, GridConfig, MarketMakerConfig, MomentumConfig, TriangularArbitrageConfig,
};

use crate::csv_loader::load_price_csv;
use crate::error::BacktestError;
use crate::report::BacktestReport;
use crate::simulated_executor::{SimulatedExecutor, SimulatedExecutorConfig};

/// Mirrors `config.toml`'s `[backtest]` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestConfig {
    pub data_file: String,
    pub slippage_bps: u16,
    pub maker_fee_bps: u16,
    pub taker_fee_bps: u16,
    pub starting_capital_quote: f64,
    /// Approximate daily interest rate (in basis points) charged on the
    /// borrowed portion of a leveraged (margin/futures) position - see
    /// `apply_fill` for exactly how "borrowed portion" is estimated. `0`
    /// for a spot-only backtest.
    #[serde(default)]
    pub margin_interest_bps_per_day: u32,
    /// Which market type the synthetic backtest asset trades as - affects
    /// whether `margin_interest_bps_per_day` and the tier's leverage cap
    /// apply. Historical OHLC data doesn't carry a real funding-rate
    /// series, so `Futures` backtests use `0.0` funding throughout -
    /// documented as a real gap, not silently assumed away.
    #[serde(default)]
    pub market_type: MarketType,
    #[serde(default = "default_leverage")]
    pub leverage: f64,
}

fn default_leverage() -> f64 {
    1.0
}

pub struct BacktestParams<'a> {
    pub strategy_name: &'a str,
    pub momentum: &'a MomentumConfig,
    pub grid: &'a GridConfig,
    pub market_maker: &'a MarketMakerConfig,
    pub triangular_arbitrage: &'a TriangularArbitrageConfig,
    pub funding_carry: &'a FundingCarryConfig,
    pub risk: &'a RiskConfig,
    pub tiers: &'a HashMap<RiskTier, TierConfig>,
    pub backtest: &'a BacktestConfig,
    pub data_path: &'a Path,
}

pub fn run(params: &BacktestParams) -> Result<BacktestReport, BacktestError> {
    let pair = Pair::from("BACKTEST/QUOTE"); // single synthetic asset id for the whole run
    let ticks = load_price_csv(params.data_path, pair.clone())?;

    let mut strategy = build_strategy(
        params.strategy_name,
        params.momentum,
        params.grid,
        params.market_maker,
        params.triangular_arbitrage,
        params.funding_carry,
    )?;
    let mut risk_mgr = RiskManager::new(
        params.risk.clone(),
        params.tiers.clone(),
        params.backtest.slippage_bps,
        params.backtest.starting_capital_quote,
    );
    let executor = SimulatedExecutor::new(SimulatedExecutorConfig {
        slippage_bps: params.backtest.slippage_bps,
        maker_fee_bps: params.backtest.maker_fee_bps,
        taker_fee_bps: params.backtest.taker_fee_bps,
    });

    let pair_meta = PairMeta {
        pair: pair.clone(),
        market_type: params.backtest.market_type,
        risk_tier: RiskTier::from(params.backtest.market_type),
        leverage: params.backtest.leverage,
    };

    let mut equity_curve = Vec::with_capacity(ticks.len());
    let mut trade_pnls = Vec::new();
    let mut total_carrying_cost_quote = 0.0;
    // (entry_price, entry_ts, leverage) - leverage tracked per-position so
    // the carrying-cost estimate at close time uses what was actually in
    // effect when the position was opened.
    let mut open_entry: Option<(f64, i64, f64)> = None;
    let mut breaker_trips: u32 = 0;

    for tick in &ticks {
        // Protective exits (stop-loss/take-profit/liquidation-guard) fire
        // on every tick regardless of whether the strategy emits anything
        // this tick.
        let protective_orders = risk_mgr.on_price_tick(tick);
        for order in protective_orders {
            let leverage = order.pair_meta.leverage;
            let fill = executor.fill(&order, tick.price);
            apply_fill(&fill, leverage, &mut risk_mgr, strategy.as_mut(), &mut open_entry, &mut trade_pnls, &mut total_carrying_cost_quote, params.backtest.margin_interest_bps_per_day);
        }

        // Strategy-driven signals, gated by risk - the only path a trade can take.
        let signals = strategy.on_price_tick(tick);
        for signal in signals {
            if let RiskDecision::Approved(order) = risk_mgr.evaluate_signal(&signal, &pair_meta, tick.price) {
                let leverage = order.pair_meta.leverage;
                let fill = executor.fill(&order, tick.price);
                apply_fill(&fill, leverage, &mut risk_mgr, strategy.as_mut(), &mut open_entry, &mut trade_pnls, &mut total_carrying_cost_quote, params.backtest.margin_interest_bps_per_day);
            }
        }

        for event in risk_mgr.drain_events() {
            if matches!(event, AppEvent::CircuitBreakerTripped { .. }) {
                breaker_trips += 1;
            }
        }

        equity_curve.push((tick.ts, risk_mgr.equity_quote()));
    }

    Ok(BacktestReport::compute(equity_curve, &trade_pnls, breaker_trips, total_carrying_cost_quote))
}

#[allow(clippy::too_many_arguments)]
fn apply_fill(
    fill: &Fill,
    leverage: f64,
    risk_mgr: &mut RiskManager,
    strategy: &mut dyn Strategy,
    open_entry: &mut Option<(f64, i64, f64)>,
    trade_pnls: &mut Vec<f64>,
    total_carrying_cost_quote: &mut f64,
    margin_interest_bps_per_day: u32,
) {
    match fill.side {
        Side::Buy => {
            *open_entry = Some((fill.price, fill.ts, leverage));
        }
        Side::Sell => {
            if let Some((entry_price, entry_ts, entry_leverage)) = open_entry.take() {
                let carrying_cost = margin_carrying_cost(
                    fill.qty, entry_price, entry_leverage, entry_ts, fill.ts, margin_interest_bps_per_day,
                );
                *total_carrying_cost_quote += carrying_cost;
                let pnl = (fill.price - entry_price) * fill.qty - fill.fee_quote - carrying_cost;
                trade_pnls.push(pnl);
            }
        }
    }
    risk_mgr.on_fill(fill);
    strategy.on_fill(fill);
}

/// Approximate interest cost for holding a leveraged position: charges
/// `margin_interest_bps_per_day` on the estimated *borrowed* portion of the
/// position's entry notional - `notional * (leverage - 1) / leverage` - for
/// the number of days it was held. `0.0` whenever `leverage <= 1.0` (spot
/// positions aren't margined). Deliberately simple and documented as such
/// (see `BacktestConfig::margin_interest_bps_per_day`) - it ignores
/// intraday compounding and any real broker minimums/tiers.
fn margin_carrying_cost(qty: f64, entry_price: f64, leverage: f64, entry_ts: i64, exit_ts: i64, margin_interest_bps_per_day: u32) -> f64 {
    if leverage <= 1.0 || margin_interest_bps_per_day == 0 {
        return 0.0;
    }
    let notional = qty * entry_price;
    let borrowed = notional * (leverage - 1.0) / leverage;
    let days_held = ((exit_ts - entry_ts) as f64 / 86_400.0).max(0.0);
    let daily_rate = margin_interest_bps_per_day as f64 / 10_000.0;
    borrowed * daily_rate * days_held
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
            maker_fee_bps: 25,
            taker_fee_bps: 40,
            starting_capital_quote: 100_000.0,
            margin_interest_bps_per_day: 0,
            market_type: MarketType::Spot,
            leverage: 1.0,
        }
    }

    struct AllConfigs {
        momentum: MomentumConfig,
        grid: GridConfig,
        market_maker: MarketMakerConfig,
        triangular_arbitrage: TriangularArbitrageConfig,
        funding_carry: FundingCarryConfig,
    }
    fn all_configs() -> AllConfigs {
        AllConfigs {
            momentum: MomentumConfig::default(),
            grid: GridConfig::default(),
            market_maker: MarketMakerConfig::default(),
            triangular_arbitrage: TriangularArbitrageConfig::default(),
            funding_carry: FundingCarryConfig::default(),
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

        let c = all_configs();
        let risk_cfg = RiskConfig::default();
        let tiers = default_tiers();
        let bt_cfg = base_backtest_cfg();

        let params = BacktestParams {
            strategy_name: "momentum",
            momentum: &c.momentum, grid: &c.grid, market_maker: &c.market_maker,
            triangular_arbitrage: &c.triangular_arbitrage, funding_carry: &c.funding_carry,
            risk: &risk_cfg, tiers: &tiers, backtest: &bt_cfg, data_path: &path,
        };
        let report = run(&params).unwrap();
        assert_eq!(report.equity_curve.len(), prices.len());
        assert!(report.starting_equity_quote > 0.0);

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

        let c = all_configs();
        let risk_cfg = RiskConfig::default();
        let tiers = default_tiers();
        let bt_cfg = base_backtest_cfg();

        let loose = MomentumConfig { threshold_pct: 0.05, cooldown_secs: 0, ..MomentumConfig::default() };
        let strict = MomentumConfig { threshold_pct: 20.0, cooldown_secs: 0, ..MomentumConfig::default() };

        let report_loose = run(&BacktestParams {
            strategy_name: "momentum", momentum: &loose, grid: &c.grid, market_maker: &c.market_maker,
            triangular_arbitrage: &c.triangular_arbitrage, funding_carry: &c.funding_carry,
            risk: &risk_cfg, tiers: &tiers, backtest: &bt_cfg, data_path: &path,
        }).unwrap();
        let report_strict = run(&BacktestParams {
            strategy_name: "momentum", momentum: &strict, grid: &c.grid, market_maker: &c.market_maker,
            triangular_arbitrage: &c.triangular_arbitrage, funding_carry: &c.funding_carry,
            risk: &risk_cfg, tiers: &tiers, backtest: &bt_cfg, data_path: &path,
        }).unwrap();

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

        let c = all_configs();
        let risk_cfg = RiskConfig::default();
        let tiers = default_tiers();
        let bt_cfg = base_backtest_cfg();

        let result = run(&BacktestParams {
            strategy_name: "not_a_real_strategy", momentum: &c.momentum, grid: &c.grid, market_maker: &c.market_maker,
            triangular_arbitrage: &c.triangular_arbitrage, funding_carry: &c.funding_carry,
            risk: &risk_cfg, tiers: &tiers, backtest: &bt_cfg, data_path: &path,
        });
        assert!(result.is_err());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn margin_carrying_cost_is_zero_for_unleveraged_positions() {
        assert_eq!(margin_carrying_cost(10.0, 100.0, 1.0, 0, 86_400, 100), 0.0);
    }

    #[test]
    fn margin_carrying_cost_scales_with_leverage_and_holding_time() {
        // 2x leverage, 1000 notional, held 1 day at 100bps/day.
        let one_day = margin_carrying_cost(10.0, 100.0, 2.0, 0, 86_400, 100);
        let two_days = margin_carrying_cost(10.0, 100.0, 2.0, 0, 2 * 86_400, 100);
        assert!(one_day > 0.0);
        assert!((two_days - one_day * 2.0).abs() < 1e-9, "cost should scale linearly with days held");

        let higher_leverage = margin_carrying_cost(10.0, 100.0, 5.0, 0, 86_400, 100);
        assert!(higher_leverage > one_day, "more leverage should borrow a larger fraction of notional");
    }

    #[test]
    fn a_leveraged_backtest_run_produces_a_nonzero_total_carrying_cost() {
        let path = std::env::temp_dir().join("bt_engine_margin_cost.csv");
        let mut prices = vec![100.0; 25];
        for i in 0..20 {
            prices.push(100.0 + i as f64 * 2.0);
        }
        for i in 0..10 {
            prices.push(140.0 - i as f64 * 3.0);
        }
        synthetic_csv(&path, &prices);

        let c = all_configs();
        let risk_cfg = RiskConfig::default();
        let tiers = default_tiers();
        let bt_cfg = BacktestConfig {
            margin_interest_bps_per_day: 50,
            market_type: MarketType::Margin,
            leverage: 2.0,
            ..base_backtest_cfg()
        };

        let report = run(&BacktestParams {
            strategy_name: "momentum", momentum: &c.momentum, grid: &c.grid, market_maker: &c.market_maker,
            triangular_arbitrage: &c.triangular_arbitrage, funding_carry: &c.funding_carry,
            risk: &risk_cfg, tiers: &tiers, backtest: &bt_cfg, data_path: &path,
        }).unwrap();
        assert!(report.total_carrying_cost_quote > 0.0, "a leveraged run with closed trades should accrue carrying cost");

        std::fs::remove_file(&path).ok();
    }
}
