//! Funding-rate carry strategy for Kraken Futures perpetuals: goes long a
//! perpetual contract when its funding rate is persistently negative
//! (shorts are paying longs to hold their position, so a long collects
//! that payment) and exits once funding turns unfavorable again. This is
//! the derivatives-specific edge that justifies including futures at all
//! in this build - see `PriceTick::funding_rate` (populated for futures
//! ticks only) and `execution::kraken_futures::FuturesTicker`.
//!
//! **Honest scope limit:** a "real" funding-rate carry trade is
//! delta-neutral - a long/short perpetual position hedged against an
//! offsetting spot position, so the trader collects funding with close to
//! zero price-directional risk. This pipeline approves one order at a
//! time and has no mechanism for a simultaneous hedging leg, so this
//! strategy is a funding-informed **directional tilt** on the perpetual
//! itself, not a hedged carry trade - real price risk remains. It also
//! only ever goes long (this build is long-only, see `risk`'s docs): when
//! funding favors shorting instead, the strategy just exits any existing
//! long rather than attempting to open a short it can't represent.

use bot_core::{Fill, PriceTick, Side, Signal, Strategy};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundingCarryConfig {
    /// The Kraken Futures perpetual symbol this strategy watches, e.g.
    /// `"PI_XBTUSD"`.
    pub pair: String,
    /// Minimum |hourly funding rate|, in percent, before acting on it.
    /// Kraken caps hourly funding at ±0.50%; set this comfortably below
    /// that but above noise.
    pub min_funding_rate_pct: f64,
    pub signal_strength: f64,
}

impl Default for FundingCarryConfig {
    fn default() -> Self {
        Self { pair: "PI_XBTUSD".to_string(), min_funding_rate_pct: 0.01, signal_strength: 0.4 }
    }
}

pub struct FundingCarryStrategy {
    cfg: FundingCarryConfig,
    is_long: bool,
}

impl FundingCarryStrategy {
    pub fn new(cfg: FundingCarryConfig) -> Self {
        assert!(cfg.min_funding_rate_pct > 0.0, "min_funding_rate_pct must be > 0");
        Self { cfg, is_long: false }
    }
}

impl Strategy for FundingCarryStrategy {
    fn name(&self) -> &str {
        "funding_carry"
    }

    fn on_price_tick(&mut self, tick: &PriceTick) -> Vec<Signal> {
        if tick.pair.as_str() != self.cfg.pair {
            return vec![];
        }
        let Some(funding_rate) = tick.funding_rate else { return vec![] };
        let funding_pct = funding_rate * 100.0;

        // Negative funding: shorts pay longs -> go long to collect it.
        if !self.is_long && funding_pct <= -self.cfg.min_funding_rate_pct {
            return vec![Signal {
                side: Side::Buy,
                pair: tick.pair.clone(),
                strength: self.cfg.signal_strength,
                reason: format!("funding_carry: funding {funding_pct:+.4}%/hr favors holding long"),
                strategy: self.name().to_string(),
                ts: tick.ts,
            }];
        }

        // Funding flipped positive (longs now pay shorts) while we're
        // long - exit; staying long would mean paying away the funding
        // this strategy exists to collect.
        if self.is_long && funding_pct >= self.cfg.min_funding_rate_pct {
            return vec![Signal {
                side: Side::Sell,
                pair: tick.pair.clone(),
                strength: 1.0,
                reason: format!("funding_carry: funding flipped to {funding_pct:+.4}%/hr, exiting"),
                strategy: self.name().to_string(),
                ts: tick.ts,
            }];
        }

        vec![]
    }

    fn on_fill(&mut self, fill: &Fill) {
        if fill.pair.as_str() != self.cfg.pair {
            return;
        }
        self.is_long = matches!(fill.side, Side::Buy);
    }

    fn reset(&mut self) {
        self.is_long = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::{MarketType, OrderReason, Pair};

    fn tick(pair: &str, funding_rate: Option<f64>, ts: i64) -> PriceTick {
        PriceTick { pair: Pair::from(pair), price: 50_000.0, funding_rate, ts }
    }

    fn cfg() -> FundingCarryConfig {
        FundingCarryConfig { pair: "PI_XBTUSD".to_string(), min_funding_rate_pct: 0.01, signal_strength: 0.4 }
    }

    #[test]
    fn a_tick_for_a_different_pair_is_ignored() {
        let mut s = FundingCarryStrategy::new(cfg());
        assert!(s.on_price_tick(&tick("PI_ETHUSD", Some(-0.001), 0)).is_empty());
    }

    #[test]
    fn a_tick_with_no_funding_rate_is_ignored() {
        let mut s = FundingCarryStrategy::new(cfg());
        assert!(s.on_price_tick(&tick("PI_XBTUSD", None, 0)).is_empty());
    }

    #[test]
    fn negative_funding_signals_a_buy_when_flat() {
        let mut s = FundingCarryStrategy::new(cfg());
        // -0.02% > 0.01% threshold in magnitude.
        let signals = s.on_price_tick(&tick("PI_XBTUSD", Some(-0.0002), 0));
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].side, Side::Buy);
    }

    #[test]
    fn does_not_re_signal_a_buy_while_already_long() {
        let mut s = FundingCarryStrategy::new(cfg());
        s.on_fill(&Fill {
            pair: Pair::from("PI_XBTUSD"), side: Side::Buy, qty: 1.0, price: 50_000.0, quote_amount: 50_000.0,
            fee_quote: 0.0, funding_paid_quote: 0.0, slippage_bps: None, market_type: MarketType::Futures,
            strategy: "funding_carry".into(), reason: OrderReason::Strategy, order_id: None, dry_run: true, ts: 0,
        });
        let signals = s.on_price_tick(&tick("PI_XBTUSD", Some(-0.0002), 1));
        assert!(signals.is_empty());
    }

    #[test]
    fn funding_flip_to_positive_exits_an_existing_long() {
        let mut s = FundingCarryStrategy::new(cfg());
        s.on_fill(&Fill {
            pair: Pair::from("PI_XBTUSD"), side: Side::Buy, qty: 1.0, price: 50_000.0, quote_amount: 50_000.0,
            fee_quote: 0.0, funding_paid_quote: 0.0, slippage_bps: None, market_type: MarketType::Futures,
            strategy: "funding_carry".into(), reason: OrderReason::Strategy, order_id: None, dry_run: true, ts: 0,
        });
        let signals = s.on_price_tick(&tick("PI_XBTUSD", Some(0.0002), 1));
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].side, Side::Sell);
    }

    #[test]
    fn small_funding_within_the_threshold_produces_no_signal() {
        let mut s = FundingCarryStrategy::new(cfg());
        assert!(s.on_price_tick(&tick("PI_XBTUSD", Some(-0.00001), 0)).is_empty());
    }

    #[test]
    fn reset_clears_the_held_flag() {
        let mut s = FundingCarryStrategy::new(cfg());
        s.on_fill(&Fill {
            pair: Pair::from("PI_XBTUSD"), side: Side::Buy, qty: 1.0, price: 50_000.0, quote_amount: 50_000.0,
            fee_quote: 0.0, funding_paid_quote: 0.0, slippage_bps: None, market_type: MarketType::Futures,
            strategy: "funding_carry".into(), reason: OrderReason::Strategy, order_id: None, dry_run: true, ts: 0,
        });
        s.reset();
        // No longer considered long, so negative funding signals a fresh buy again.
        let signals = s.on_price_tick(&tick("PI_XBTUSD", Some(-0.0002), 1));
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].side, Side::Buy);
    }
}
