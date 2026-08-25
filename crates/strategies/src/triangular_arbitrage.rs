//! Triangular-arbitrage signal generator: watches three correlated Kraken
//! pairs (e.g. `XBT/USD`, `ETH/USD`, `ETH/XBT`) and compares the "cross"
//! pair's actual price against the rate implied by the other two
//! (`implied = pair_b / pair_a`). A large enough gap is a real,
//! single-exchange mispricing that a bonding-curve-only bot couldn't
//! exploit - it needs an actual multi-pair order book, which is exactly
//! what Kraken (and centralized exchanges generally) provide.
//!
//! **Honest scope limit:** genuine triangular arbitrage profits from
//! executing all three legs together, atomically, so the mispricing is
//! captured regardless of which direction it later corrects. This
//! pipeline approves and routes one order at a time (`Signal` ->
//! `RiskManager` -> one `ApprovedOrder`), so this strategy instead trades
//! only the cross pair itself as a single-leg relative-value signal: buy
//! it when it's cheap relative to its two legs, sell (exit) it when it's
//! rich. That's a real, directional edge but **not** risk-free arbitrage -
//! documented plainly here and in the README rather than overclaimed.

use bot_core::{Pair, PriceTick, Side, Signal, Strategy};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriangularArbitrageConfig {
    /// First leg, e.g. `"XBT/USD"`.
    pub pair_a: String,
    /// Second leg, e.g. `"ETH/USD"`.
    pub pair_b: String,
    /// The cross pair this strategy actually trades, e.g. `"ETH/XBT"`.
    /// Its implied fair price is `pair_b / pair_a`.
    pub pair_c: String,
    /// Minimum absolute deviation, in percent, between `pair_c`'s actual
    /// price and its implied price before a signal fires. Should be set
    /// comfortably above round-trip fees, since a real mispricing this
    /// small is more likely fee noise than a tradeable gap.
    pub min_mispricing_pct: f64,
    pub signal_strength: f64,
}

impl Default for TriangularArbitrageConfig {
    fn default() -> Self {
        Self {
            pair_a: "XBT/USD".to_string(),
            pair_b: "ETH/USD".to_string(),
            pair_c: "ETH/XBT".to_string(),
            min_mispricing_pct: 0.5,
            signal_strength: 0.5,
        }
    }
}

pub struct TriangularArbitrageStrategy {
    cfg: TriangularArbitrageConfig,
    price_a: Option<f64>,
    price_b: Option<f64>,
    price_c: Option<f64>,
}

impl TriangularArbitrageStrategy {
    pub fn new(cfg: TriangularArbitrageConfig) -> Self {
        assert!(cfg.min_mispricing_pct > 0.0, "min_mispricing_pct must be > 0");
        Self { cfg, price_a: None, price_b: None, price_c: None }
    }

    /// The rate `pair_c` "should" trade at if it were perfectly consistent
    /// with `pair_a`/`pair_b`. `None` until all three legs have been seen
    /// at least once.
    fn implied_price_c(&self) -> Option<f64> {
        let (a, b) = (self.price_a?, self.price_b?);
        if a <= 0.0 {
            return None;
        }
        Some(b / a)
    }
}

impl Strategy for TriangularArbitrageStrategy {
    fn name(&self) -> &str {
        "triangular_arbitrage"
    }

    fn on_price_tick(&mut self, tick: &PriceTick) -> Vec<Signal> {
        let symbol = tick.pair.as_str();
        if symbol == self.cfg.pair_a {
            self.price_a = Some(tick.price);
        } else if symbol == self.cfg.pair_b {
            self.price_b = Some(tick.price);
        } else if symbol == self.cfg.pair_c {
            self.price_c = Some(tick.price);
        } else {
            return vec![]; // a pair this strategy isn't watching
        }

        let (Some(implied), Some(actual)) = (self.implied_price_c(), self.price_c) else {
            return vec![]; // not all three legs observed yet
        };
        if implied <= 0.0 {
            return vec![];
        }
        let deviation_pct = (actual - implied) / implied * 100.0;
        if deviation_pct.abs() < self.cfg.min_mispricing_pct {
            return vec![];
        }

        // pair_c trading rich vs. implied -> sell it (exit-only in this
        // long-only build, see module docs); trading cheap -> buy it.
        let side = if deviation_pct > 0.0 { Side::Sell } else { Side::Buy };
        vec![Signal {
            side,
            pair: Pair::from(self.cfg.pair_c.clone()),
            strength: self.cfg.signal_strength,
            reason: format!(
                "triangular_arbitrage: {} implied {implied:.8} vs actual {actual:.8} ({deviation_pct:+.3}%)",
                self.cfg.pair_c
            ),
            strategy: self.name().to_string(),
            ts: tick.ts,
        }]
    }

    fn reset(&mut self) {
        self.price_a = None;
        self.price_b = None;
        self.price_c = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick(symbol: &str, price: f64, ts: i64) -> PriceTick {
        PriceTick { pair: Pair::from(symbol), price, funding_rate: None, ts }
    }

    fn strat() -> TriangularArbitrageStrategy {
        TriangularArbitrageStrategy::new(TriangularArbitrageConfig::default())
    }

    #[test]
    fn no_signal_until_all_three_legs_are_observed() {
        let mut s = strat();
        assert!(s.on_price_tick(&tick("XBT/USD", 50_000.0, 0)).is_empty());
        assert!(s.on_price_tick(&tick("ETH/USD", 2_500.0, 1)).is_empty());
        // Now the third leg arrives, exactly at the implied rate - no
        // mispricing, so still no signal.
        assert!(s.on_price_tick(&tick("ETH/XBT", 0.05, 2)).is_empty());
    }

    #[test]
    fn a_rich_cross_pair_signals_a_sell() {
        let mut s = strat();
        s.on_price_tick(&tick("XBT/USD", 50_000.0, 0));
        s.on_price_tick(&tick("ETH/USD", 2_500.0, 1));
        // implied ETH/XBT = 2500/50000 = 0.05; actual 0.052 is +4% rich.
        let signals = s.on_price_tick(&tick("ETH/XBT", 0.052, 2));
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].side, Side::Sell);
        assert_eq!(signals[0].pair, Pair::from("ETH/XBT"));
    }

    #[test]
    fn a_cheap_cross_pair_signals_a_buy() {
        let mut s = strat();
        s.on_price_tick(&tick("XBT/USD", 50_000.0, 0));
        s.on_price_tick(&tick("ETH/USD", 2_500.0, 1));
        // actual 0.048 is below the 0.05 implied rate.
        let signals = s.on_price_tick(&tick("ETH/XBT", 0.048, 2));
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].side, Side::Buy);
    }

    #[test]
    fn a_deviation_below_the_threshold_produces_no_signal() {
        let mut s = strat();
        s.on_price_tick(&tick("XBT/USD", 50_000.0, 0));
        s.on_price_tick(&tick("ETH/USD", 2_500.0, 1));
        // 0.0501 vs implied 0.05 is only +0.2% - below the 0.5% threshold.
        assert!(s.on_price_tick(&tick("ETH/XBT", 0.0501, 2)).is_empty());
    }

    #[test]
    fn ticks_for_an_unrelated_pair_are_ignored() {
        let mut s = strat();
        s.on_price_tick(&tick("XBT/USD", 50_000.0, 0));
        s.on_price_tick(&tick("ETH/USD", 2_500.0, 1));
        assert!(s.on_price_tick(&tick("DOGE/USD", 0.1, 2)).is_empty());
    }

    #[test]
    fn reset_forgets_all_three_legs() {
        let mut s = strat();
        s.on_price_tick(&tick("XBT/USD", 50_000.0, 0));
        s.on_price_tick(&tick("ETH/USD", 2_500.0, 1));
        s.reset();
        // Only the cross pair is now known again - no signal, since the
        // other two legs were forgotten.
        assert!(s.on_price_tick(&tick("ETH/XBT", 0.1, 2)).is_empty());
    }
}
