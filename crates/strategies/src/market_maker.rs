//! Market-making strategy: the concrete "capture the spread" mechanism
//! behind the README's "how to make the most money on Kraken" section.
//! This strategy only ever decides *when* to be long vs. flat; the actual
//! maker-fee capture happens in `execution::Executor`, which places every
//! `OrderReason::Strategy` order as a post-only limit joining the current
//! best bid (buy) / best ask (sell) - so a round trip through this
//! strategy earns the bid-ask spread plus the maker fee tier, rather than
//! paying it away on both legs the way a pair of market orders would.
//!
//! v1 is single-pair and long-only (buy low, sell once a configured
//! minimum spread is captured, repeat) - it does not simultaneously quote
//! both sides of the book the way a "real" market maker's bot would; see
//! README for that documented scope limit.

use bot_core::{Fill, PriceTick, Side, Signal, Strategy};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketMakerConfig {
    /// Minimum % gain from entry before the strategy signals an exit -
    /// this needs to clear round-trip fees (even at the maker rate) to be
    /// worth taking; see README's fee-tier numbers.
    pub target_spread_pct: f64,
    /// Fixed confidence passed on every signal - sizing itself is `risk`'s
    /// job, not this strategy's.
    pub signal_strength: f64,
    /// Minimum seconds between two "enter" signals while flat, so a flat
    /// strategy doesn't flood the signal channel every tick waiting for a
    /// pending buy to resolve.
    pub requote_cooldown_secs: i64,
}

impl Default for MarketMakerConfig {
    fn default() -> Self {
        Self { target_spread_pct: 0.3, signal_strength: 0.5, requote_cooldown_secs: 5 }
    }
}

pub struct MarketMakerStrategy {
    cfg: MarketMakerConfig,
    entry_price: Option<f64>,
    last_enter_signal_ts: Option<i64>,
}

impl MarketMakerStrategy {
    pub fn new(cfg: MarketMakerConfig) -> Self {
        assert!(cfg.target_spread_pct > 0.0, "target_spread_pct must be > 0");
        Self { cfg, entry_price: None, last_enter_signal_ts: None }
    }
}

impl Strategy for MarketMakerStrategy {
    fn name(&self) -> &str {
        "market_maker"
    }

    fn on_price_tick(&mut self, tick: &PriceTick) -> Vec<Signal> {
        match self.entry_price {
            None => {
                if let Some(last) = self.last_enter_signal_ts {
                    if tick.ts - last < self.cfg.requote_cooldown_secs {
                        return vec![];
                    }
                }
                self.last_enter_signal_ts = Some(tick.ts);
                vec![Signal {
                    side: Side::Buy,
                    pair: tick.pair.clone(),
                    strength: self.cfg.signal_strength,
                    reason: "market_maker: flat, quoting the bid to open a position".to_string(),
                    strategy: self.name().to_string(),
                    ts: tick.ts,
                }]
            }
            Some(entry) => {
                let gain_pct = if entry > 0.0 { (tick.price - entry) / entry * 100.0 } else { 0.0 };
                if gain_pct < self.cfg.target_spread_pct {
                    return vec![];
                }
                vec![Signal {
                    side: Side::Sell,
                    pair: tick.pair.clone(),
                    strength: 1.0,
                    reason: format!("market_maker: captured {gain_pct:.4}% (target {:.4}%)", self.cfg.target_spread_pct),
                    strategy: self.name().to_string(),
                    ts: tick.ts,
                }]
            }
        }
    }

    fn on_fill(&mut self, fill: &Fill) {
        match fill.side {
            Side::Buy => self.entry_price = Some(fill.price),
            Side::Sell => self.entry_price = None,
        }
    }

    fn reset(&mut self) {
        self.entry_price = None;
        self.last_enter_signal_ts = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::{MarketType, OrderReason, Pair};

    fn tick(pair: &Pair, price: f64, ts: i64) -> PriceTick {
        PriceTick { pair: pair.clone(), price, funding_rate: None, ts }
    }

    fn cfg() -> MarketMakerConfig {
        MarketMakerConfig { target_spread_pct: 0.5, signal_strength: 0.5, requote_cooldown_secs: 0 }
    }

    #[test]
    fn flat_strategy_signals_a_buy() {
        let mut s = MarketMakerStrategy::new(cfg());
        let pair = Pair::from("XBT/USD");
        let signals = s.on_price_tick(&tick(&pair, 100.0, 0));
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].side, Side::Buy);
    }

    #[test]
    fn does_not_exit_before_the_target_spread_is_reached() {
        let mut s = MarketMakerStrategy::new(cfg());
        let pair = Pair::from("XBT/USD");
        s.on_fill(&Fill {
            pair: pair.clone(), side: Side::Buy, qty: 1.0, price: 100.0, quote_amount: 100.0,
            fee_quote: 0.0, funding_paid_quote: 0.0, slippage_bps: None, market_type: MarketType::Spot,
            strategy: "market_maker".into(), reason: OrderReason::Strategy, order_id: None, dry_run: true, ts: 0,
        });
        // Only 0.1% gain - below the 0.5% target.
        let signals = s.on_price_tick(&tick(&pair, 100.1, 1));
        assert!(signals.is_empty());
    }

    #[test]
    fn exits_once_the_target_spread_is_captured() {
        let mut s = MarketMakerStrategy::new(cfg());
        let pair = Pair::from("XBT/USD");
        s.on_fill(&Fill {
            pair: pair.clone(), side: Side::Buy, qty: 1.0, price: 100.0, quote_amount: 100.0,
            fee_quote: 0.0, funding_paid_quote: 0.0, slippage_bps: None, market_type: MarketType::Spot,
            strategy: "market_maker".into(), reason: OrderReason::Strategy, order_id: None, dry_run: true, ts: 0,
        });
        let signals = s.on_price_tick(&tick(&pair, 100.6, 1)); // +0.6% >= 0.5% target
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].side, Side::Sell);
    }

    #[test]
    fn requote_cooldown_suppresses_repeat_buy_signals() {
        let mut s = MarketMakerStrategy::new(MarketMakerConfig { requote_cooldown_secs: 100, ..cfg() });
        let pair = Pair::from("XBT/USD");
        let first = s.on_price_tick(&tick(&pair, 100.0, 0));
        assert_eq!(first.len(), 1);
        let second = s.on_price_tick(&tick(&pair, 100.0, 1));
        assert!(second.is_empty(), "should not requote within the cooldown window");
    }

    #[test]
    fn reset_clears_state_and_allows_a_fresh_entry() {
        let mut s = MarketMakerStrategy::new(cfg());
        let pair = Pair::from("XBT/USD");
        s.on_fill(&Fill {
            pair: pair.clone(), side: Side::Buy, qty: 1.0, price: 100.0, quote_amount: 100.0,
            fee_quote: 0.0, funding_paid_quote: 0.0, slippage_bps: None, market_type: MarketType::Spot,
            strategy: "market_maker".into(), reason: OrderReason::Strategy, order_id: None, dry_run: true, ts: 0,
        });
        s.reset();
        let signals = s.on_price_tick(&tick(&pair, 100.0, 0));
        assert_eq!(signals[0].side, Side::Buy, "after reset the strategy should be flat again");
    }
}
