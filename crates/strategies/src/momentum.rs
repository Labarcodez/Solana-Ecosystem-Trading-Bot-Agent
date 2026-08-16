//! Simple-moving-average crossover momentum strategy.
//!
//! Tracks a rolling window of prices, computes a short-window and a
//! long-window SMA on every tick, and emits a signal when the short SMA
//! crosses the long SMA by more than `threshold_pct` - not on every tick
//! where the gap happens to be wide, only on a fresh crossover, and never
//! more often than `cooldown_secs`.

use std::collections::VecDeque;

use bot_core::{PriceTick, Side, Signal, Strategy};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentumConfig {
    pub short_window: usize,
    pub long_window: usize,
    /// Minimum % separation between short and long SMA required to fire a signal.
    pub threshold_pct: f64,
    pub cooldown_secs: i64,
}

impl Default for MomentumConfig {
    fn default() -> Self {
        Self {
            short_window: 9,
            long_window: 21,
            threshold_pct: 0.5,
            cooldown_secs: 30,
        }
    }
}

pub struct MomentumStrategy {
    cfg: MomentumConfig,
    prices: VecDeque<f64>,
    last_signal_ts: Option<i64>,
    last_signal_side: Option<Side>,
}

impl MomentumStrategy {
    pub fn new(cfg: MomentumConfig) -> Self {
        assert!(
            cfg.short_window > 0 && cfg.short_window < cfg.long_window,
            "short_window must be > 0 and < long_window"
        );
        let prices = VecDeque::with_capacity(cfg.long_window);
        Self {
            cfg,
            prices,
            last_signal_ts: None,
            last_signal_side: None,
        }
    }

    fn sma(&self, window: usize) -> f64 {
        let n = self.prices.len();
        let slice_start = n - window;
        let sum: f64 = self.prices.iter().skip(slice_start).sum();
        sum / window as f64
    }
}

impl Strategy for MomentumStrategy {
    fn name(&self) -> &str {
        "momentum"
    }

    fn on_price_tick(&mut self, tick: &PriceTick) -> Vec<Signal> {
        self.prices.push_back(tick.price);
        if self.prices.len() > self.cfg.long_window {
            self.prices.pop_front();
        }
        if self.prices.len() < self.cfg.long_window {
            return vec![]; // not enough history yet
        }

        let short_sma = self.sma(self.cfg.short_window);
        let long_sma = self.sma(self.cfg.long_window);
        if long_sma == 0.0 {
            return vec![];
        }
        let separation_pct = (short_sma - long_sma) / long_sma * 100.0;

        let candidate_side = if separation_pct >= self.cfg.threshold_pct {
            Some(Side::Buy)
        } else if separation_pct <= -self.cfg.threshold_pct {
            Some(Side::Sell)
        } else {
            None
        };

        let Some(side) = candidate_side else {
            return vec![];
        };

        // Only a *fresh* crossover fires - i.e. the detected side changed
        // since the last signal we emitted.
        if self.last_signal_side == Some(side) {
            return vec![];
        }
        if let Some(last_ts) = self.last_signal_ts {
            if tick.ts - last_ts < self.cfg.cooldown_secs {
                return vec![];
            }
        }

        self.last_signal_ts = Some(tick.ts);
        self.last_signal_side = Some(side);

        // Scale confidence with how far past the threshold we are, capped at 1.0.
        let strength = (separation_pct.abs() / (self.cfg.threshold_pct * 4.0)).min(1.0);

        vec![Signal {
            side,
            mint: tick.mint,
            strength,
            reason: format!(
                "SMA{} {:.6} vs SMA{} {:.6} ({:+.2}%)",
                self.cfg.short_window, short_sma, self.cfg.long_window, long_sma, separation_pct
            ),
            strategy: self.name().to_string(),
            ts: tick.ts,
        }]
    }

    fn reset(&mut self) {
        self.prices.clear();
        self.last_signal_ts = None;
        self.last_signal_side = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::Pubkey;

    fn tick(price: f64, ts: i64) -> PriceTick {
        PriceTick {
            mint: Pubkey::new_unique(),
            price,
            ts,
        }
    }

    #[test]
    fn no_signal_before_enough_history() {
        let cfg = MomentumConfig {
            short_window: 3,
            long_window: 5,
            threshold_pct: 0.1,
            cooldown_secs: 0,
        };
        let mut s = MomentumStrategy::new(cfg);
        for i in 0..4 {
            assert!(s.on_price_tick(&tick(100.0 + i as f64, i as i64)).is_empty());
        }
    }

    #[test]
    fn rising_prices_emit_buy_once() {
        let cfg = MomentumConfig {
            short_window: 2,
            long_window: 4,
            threshold_pct: 0.1,
            cooldown_secs: 0,
        };
        let mut s = MomentumStrategy::new(cfg);
        let mint = Pubkey::new_unique();
        let mut signals = vec![];
        // Flat prices to fill history, then a sharp rise to trigger the crossover.
        for (i, price) in [100.0, 100.0, 100.0, 100.0, 110.0, 120.0, 130.0]
            .into_iter()
            .enumerate()
        {
            let t = PriceTick { mint, price, ts: i as i64 };
            signals.extend(s.on_price_tick(&t));
        }
        assert_eq!(signals.len(), 1, "expected exactly one buy signal, got {signals:?}");
        assert_eq!(signals[0].side, Side::Buy);
    }

    #[test]
    fn cooldown_suppresses_repeat_signals() {
        let cfg = MomentumConfig {
            short_window: 2,
            long_window: 4,
            threshold_pct: 0.1,
            cooldown_secs: 1000,
        };
        let mut s = MomentumStrategy::new(cfg);
        let mint = Pubkey::new_unique();
        let mut signals = vec![];
        for (i, price) in [100.0, 100.0, 100.0, 100.0, 110.0, 120.0, 90.0, 80.0]
            .into_iter()
            .enumerate()
        {
            let t = PriceTick { mint, price, ts: i as i64 };
            signals.extend(s.on_price_tick(&t));
        }
        // Even though the trend reverses hard, the long cooldown should
        // suppress a second signal within the window.
        assert_eq!(signals.len(), 1);
    }

    #[test]
    fn reset_clears_state() {
        let cfg = MomentumConfig::default();
        let mut s = MomentumStrategy::new(cfg.clone());
        let mint = Pubkey::new_unique();
        for i in 0..cfg.long_window {
            s.on_price_tick(&PriceTick { mint, price: 100.0, ts: i as i64 });
        }
        assert_eq!(s.prices.len(), cfg.long_window);
        s.reset();
        assert_eq!(s.prices.len(), 0);
        assert!(s.last_signal_ts.is_none());
    }

    #[test]
    fn varying_threshold_changes_behavior() {
        // Same price path, different thresholds -> different signal counts.
        // This is the same honesty property the backtester acceptance check
        // relies on: parameters must actually change output.
        let prices = [100.0, 100.0, 100.0, 100.0, 101.0, 102.0, 103.0];
        let mint = Pubkey::new_unique();

        let mut loose = MomentumStrategy::new(MomentumConfig {
            short_window: 2,
            long_window: 4,
            threshold_pct: 0.05,
            cooldown_secs: 0,
        });
        let mut strict = MomentumStrategy::new(MomentumConfig {
            short_window: 2,
            long_window: 4,
            threshold_pct: 50.0,
            cooldown_secs: 0,
        });

        let mut loose_signals = 0;
        let mut strict_signals = 0;
        for (i, price) in prices.into_iter().enumerate() {
            let t = PriceTick { mint, price, ts: i as i64 };
            loose_signals += loose.on_price_tick(&t).len();
            strict_signals += strict.on_price_tick(&t).len();
        }
        assert!(loose_signals > strict_signals);
    }
}
