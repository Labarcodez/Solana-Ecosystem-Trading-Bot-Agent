//! Grid strategy: fixed price levels spaced `grid_step_pct` apart around a
//! base price. Crossing a level while falling buys at that level; crossing
//! it again while rising, if we're still holding it, sells. Levels are
//! tracked individually so the strategy can hold several at once.

use std::collections::HashMap;

use bot_core::{PriceTick, Side, Signal, Strategy};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridConfig {
    /// 0.0 means "use the first observed price tick as the base".
    pub base_price: f64,
    pub grid_step_pct: f64,
    pub grid_levels: usize,
}

impl Default for GridConfig {
    fn default() -> Self {
        Self {
            base_price: 0.0,
            grid_step_pct: 2.0,
            grid_levels: 5,
        }
    }
}

pub struct GridStrategy {
    cfg: GridConfig,
    base_price: Option<f64>,
    last_price: Option<f64>,
    /// level index -> currently holding a position opened at that level
    filled: HashMap<i64, bool>,
}

impl GridStrategy {
    pub fn new(cfg: GridConfig) -> Self {
        assert!(cfg.grid_levels > 0, "grid_levels must be > 0");
        assert!(cfg.grid_step_pct > 0.0, "grid_step_pct must be > 0");
        Self {
            cfg,
            base_price: None,
            last_price: None,
            filled: HashMap::new(),
        }
    }

    fn level_price(&self, base: f64, k: i64) -> f64 {
        base * (1.0 + k as f64 * self.cfg.grid_step_pct / 100.0)
    }

    /// All configured level indices, excluding 0 (the base price itself is not a level).
    fn level_indices(&self) -> impl Iterator<Item = i64> {
        let n = self.cfg.grid_levels as i64;
        (-n..=n).filter(|k| *k != 0)
    }
}

impl Strategy for GridStrategy {
    fn name(&self) -> &str {
        "grid"
    }

    fn on_price_tick(&mut self, tick: &PriceTick) -> Vec<Signal> {
        let base = match self.base_price {
            Some(b) => b,
            None => {
                let b = if self.cfg.base_price > 0.0 {
                    self.cfg.base_price
                } else {
                    tick.price
                };
                self.base_price = Some(b);
                self.last_price = Some(tick.price);
                return vec![]; // first tick only establishes the baseline
            }
        };
        let last = self.last_price.unwrap_or(tick.price);
        let cur = tick.price;
        let mut signals = Vec::new();
        let indices: Vec<i64> = self.level_indices().collect();

        if cur < last {
            // Price fell: buy any unfilled level whose price lies in (cur, last].
            for k in indices {
                let lvl = self.level_price(base, k);
                if lvl <= last && lvl > cur && !self.filled.get(&k).copied().unwrap_or(false) {
                    self.filled.insert(k, true);
                    signals.push(Signal {
                        side: Side::Buy,
                        pair: tick.pair.clone(),
                        strength: 0.8,
                        reason: format!("grid level {k} ({lvl:.6}) crossed falling"),
                        strategy: "grid".to_string(),
                        ts: tick.ts,
                    });
                }
            }
        } else if cur > last {
            // Price rose: sell any filled level whose price lies in (last, cur].
            for k in indices {
                let lvl = self.level_price(base, k);
                if lvl > last && lvl <= cur && self.filled.get(&k).copied().unwrap_or(false) {
                    self.filled.insert(k, false);
                    signals.push(Signal {
                        side: Side::Sell,
                        pair: tick.pair.clone(),
                        strength: 0.8,
                        reason: format!("grid level {k} ({lvl:.6}) crossed rising"),
                        strategy: "grid".to_string(),
                        ts: tick.ts,
                    });
                }
            }
        }

        self.last_price = Some(cur);
        signals
    }

    fn on_fill(&mut self, _fill: &bot_core::Fill) {
        // Level fill-state is already updated eagerly in on_price_tick when
        // the signal is generated; nothing further to reconcile here for v1.
    }

    fn reset(&mut self) {
        self.base_price = None;
        self.last_price = None;
        self.filled.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::Pair;

    fn tick(pair: &Pair, price: f64, ts: i64) -> PriceTick {
        PriceTick { pair: pair.clone(), price, funding_rate: None, ts }
    }

    #[test]
    fn first_tick_only_sets_baseline() {
        let mut s = GridStrategy::new(GridConfig::default());
        let pair = Pair::from("XBT/USD");
        assert!(s.on_price_tick(&tick(&pair, 100.0, 0)).is_empty());
        assert_eq!(s.base_price, Some(100.0));
    }

    #[test]
    fn falling_price_buys_a_level_then_rising_sells_it() {
        let cfg = GridConfig {
            base_price: 0.0,
            grid_step_pct: 2.0,
            grid_levels: 3,
        };
        let mut s = GridStrategy::new(cfg);
        let pair = Pair::from("XBT/USD");

        s.on_price_tick(&tick(&pair, 100.0, 0)); // base = 100
        let buys = s.on_price_tick(&tick(&pair, 97.0, 1)); // crosses -1 (98) and -2 (96)? -2 level=96, 97>96 so only -1 crossed
        assert_eq!(buys.len(), 1);
        assert_eq!(buys[0].side, Side::Buy);

        let sells = s.on_price_tick(&tick(&pair, 99.5, 2)); // crosses back up through 98
        assert_eq!(sells.len(), 1);
        assert_eq!(sells[0].side, Side::Sell);
    }

    #[test]
    fn does_not_rebuy_an_already_filled_level() {
        let cfg = GridConfig {
            base_price: 0.0,
            grid_step_pct: 2.0,
            grid_levels: 3,
        };
        let mut s = GridStrategy::new(cfg);
        let pair = Pair::from("XBT/USD");
        s.on_price_tick(&tick(&pair, 100.0, 0));
        let first = s.on_price_tick(&tick(&pair, 97.0, 1));
        assert_eq!(first.len(), 1);
        // Wiggling above and back below the same level without crossing back
        // up through it first should not re-buy.
        let second = s.on_price_tick(&tick(&pair, 97.5, 2));
        assert!(second.is_empty());
        let third = s.on_price_tick(&tick(&pair, 96.9, 3));
        assert!(third.is_empty());
    }

    #[test]
    fn reset_clears_levels_and_baseline() {
        let mut s = GridStrategy::new(GridConfig::default());
        let pair = Pair::from("XBT/USD");
        s.on_price_tick(&tick(&pair, 100.0, 0));
        s.on_price_tick(&tick(&pair, 90.0, 1));
        s.reset();
        assert!(s.base_price.is_none());
        assert!(s.filled.is_empty());
    }

    #[test]
    fn tighter_step_pct_produces_more_signals_over_same_path() {
        let pair = Pair::from("XBT/USD");
        let path = [100.0, 95.0, 90.0, 85.0, 90.0, 95.0, 100.0];

        let mut wide = GridStrategy::new(GridConfig {
            base_price: 0.0,
            grid_step_pct: 10.0,
            grid_levels: 5,
        });
        let mut tight = GridStrategy::new(GridConfig {
            base_price: 0.0,
            grid_step_pct: 1.0,
            grid_levels: 20,
        });

        let mut wide_signals = 0;
        let mut tight_signals = 0;
        for (i, price) in path.into_iter().enumerate() {
            wide_signals += wide.on_price_tick(&tick(&pair, price, i as i64)).len();
            tight_signals += tight.on_price_tick(&tick(&pair, price, i as i64)).len();
        }
        assert!(tight_signals > wide_signals);
    }
}
