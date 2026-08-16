//! Turns a raw equity curve and the list of closed-trade PnLs into the
//! summary numbers a user actually wants to see. Nothing here is
//! hardcoded - every field is computed from the `equity_curve`/`trade_pnls`
//! passed in, which is what the parameter-sensitivity acceptance check in
//! `engine.rs`'s tests relies on (change a strategy/risk parameter, the
//! trade path changes, so these numbers change too).

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct BacktestReport {
    pub starting_equity_sol: f64,
    pub final_equity_sol: f64,
    pub total_return_pct: f64,
    pub max_drawdown_pct: f64,
    pub closed_trade_count: usize,
    pub win_rate_pct: f64,
    /// Mean-over-stddev of per-sample equity returns, scaled by sqrt(N).
    /// A simplified, un-annualized proxy for Sharpe - documented as
    /// "Sharpe-style", not a claim of a rigorous risk-adjusted return
    /// figure (no risk-free rate, no annualization).
    pub sharpe_like_ratio: f64,
    pub circuit_breaker_trips: u32,
    pub equity_curve: Vec<(i64, f64)>,
}

impl BacktestReport {
    pub fn compute(equity_curve: Vec<(i64, f64)>, trade_pnls: &[f64], circuit_breaker_trips: u32) -> Self {
        let starting_equity_sol = equity_curve.first().map(|(_, e)| *e).unwrap_or(0.0);
        let final_equity_sol = equity_curve.last().map(|(_, e)| *e).unwrap_or(starting_equity_sol);

        let total_return_pct = if starting_equity_sol > 0.0 {
            (final_equity_sol - starting_equity_sol) / starting_equity_sol * 100.0
        } else {
            0.0
        };

        let max_drawdown_pct = max_drawdown(&equity_curve);
        let sharpe_like_ratio = sharpe_like(&equity_curve);

        let closed_trade_count = trade_pnls.len();
        let win_rate_pct = if closed_trade_count > 0 {
            trade_pnls.iter().filter(|p| **p > 0.0).count() as f64 / closed_trade_count as f64 * 100.0
        } else {
            0.0
        };

        Self {
            starting_equity_sol,
            final_equity_sol,
            total_return_pct,
            max_drawdown_pct,
            closed_trade_count,
            win_rate_pct,
            sharpe_like_ratio,
            circuit_breaker_trips,
            equity_curve,
        }
    }

    pub fn print_summary(&self) {
        println!("=== Backtest Report ===");
        println!("Starting equity:      {:.6} SOL", self.starting_equity_sol);
        println!("Final equity:         {:.6} SOL", self.final_equity_sol);
        println!("Total return:         {:+.2}%", self.total_return_pct);
        println!("Max drawdown:         {:.2}%", self.max_drawdown_pct);
        println!("Closed trades:        {}", self.closed_trade_count);
        println!("Win rate:             {:.2}%", self.win_rate_pct);
        println!("Sharpe-style ratio:   {:.3}", self.sharpe_like_ratio);
        println!("Circuit breaker trips:{}", self.circuit_breaker_trips);
    }

    pub fn to_json_pretty(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }
}

fn max_drawdown(curve: &[(i64, f64)]) -> f64 {
    let mut peak = f64::MIN;
    let mut max_dd = 0.0f64;
    for &(_, equity) in curve {
        if equity > peak {
            peak = equity;
        }
        if peak > 0.0 {
            let dd = (peak - equity) / peak * 100.0;
            if dd > max_dd {
                max_dd = dd;
            }
        }
    }
    max_dd
}

fn sharpe_like(curve: &[(i64, f64)]) -> f64 {
    if curve.len() < 2 {
        return 0.0;
    }
    let returns: Vec<f64> = curve
        .windows(2)
        .filter_map(|w| {
            let (_, prev) = w[0];
            let (_, cur) = w[1];
            if prev > 0.0 {
                Some((cur - prev) / prev)
            } else {
                None
            }
        })
        .collect();
    if returns.is_empty() {
        return 0.0;
    }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
    let stddev = variance.sqrt();
    if stddev == 0.0 {
        return 0.0;
    }
    (mean / stddev) * (returns.len() as f64).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_return_and_drawdown() {
        let curve = vec![(0, 10.0), (1, 12.0), (2, 8.0), (3, 11.0)];
        let report = BacktestReport::compute(curve, &[1.0, -0.5, 2.0], 0);
        assert_eq!(report.starting_equity_sol, 10.0);
        assert_eq!(report.final_equity_sol, 11.0);
        assert!((report.total_return_pct - 10.0).abs() < 1e-9);
        // Peak 12 -> trough 8 = 33.33% drawdown
        assert!((report.max_drawdown_pct - 33.333333).abs() < 1e-3);
        assert_eq!(report.closed_trade_count, 3);
        assert!((report.win_rate_pct - 66.6667).abs() < 0.01);
    }

    #[test]
    fn empty_trades_gives_zero_win_rate_not_a_panic() {
        let curve = vec![(0, 10.0), (1, 10.0)];
        let report = BacktestReport::compute(curve, &[], 0);
        assert_eq!(report.win_rate_pct, 0.0);
        assert_eq!(report.closed_trade_count, 0);
    }

    #[test]
    fn json_serialization_round_trips_shape() {
        let curve = vec![(0, 10.0), (1, 10.5)];
        let report = BacktestReport::compute(curve, &[0.5], 1);
        let json = report.to_json_pretty().unwrap();
        assert!(json.contains("total_return_pct"));
        assert!(json.contains("circuit_breaker_trips"));
    }
}
