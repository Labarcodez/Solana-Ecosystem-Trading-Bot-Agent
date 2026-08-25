//! Fills `ApprovedOrder`s against a modeled fee + slippage curve instead of
//! a real Kraken order book. This is a documented simplification (see
//! README): it models Kraken's maker/taker fee split and a flat slippage %
//! on taker fills, not real order-book depth or price impact.
//!
//! The maker/taker split mirrors `execution::Executor`'s real live policy
//! exactly: a strategy-initiated order (`OrderReason::Strategy`) is modeled
//! as the post-only maker fill it would be live - no slippage, the maker
//! fee - while a protective exit (stop-loss/take-profit/liquidation-guard)
//! is modeled as the market/taker fill it would be live - slippage applies,
//! the (higher) taker fee. This is what lets a backtest actually show the
//! "post-only saves money" effect the README's fee section describes,
//! rather than charging every fill the same rate.

use bot_core::{ApprovedOrder, Fill, OrderReason, Side};

#[derive(Debug, Clone, Copy)]
pub struct SimulatedExecutorConfig {
    /// Applied only to taker (protective-exit) fills - a resting maker
    /// order fills at its own posted price, not a worse one.
    pub slippage_bps: u16,
    pub maker_fee_bps: u16,
    pub taker_fee_bps: u16,
}

pub struct SimulatedExecutor {
    cfg: SimulatedExecutorConfig,
}

impl SimulatedExecutor {
    pub fn new(cfg: SimulatedExecutorConfig) -> Self {
        Self { cfg }
    }

    fn is_maker(order: &ApprovedOrder) -> bool {
        matches!(order.reason, OrderReason::Strategy)
    }

    /// Fill `order` at `mark_price` (the current tick price), applying
    /// slippage (taker fills only) against the trade direction and
    /// deducting the modeled fee from the notional. `order.size_quote` is
    /// treated as the exact amount of capital `risk` already reserved
    /// (buys) or the exact gross notional being sold (sells) - fee comes
    /// out of that same amount, so `Fill.quote_amount` always matches what
    /// `RiskManager` expects to move in/out of `capital_quote`.
    pub fn fill(&self, order: &ApprovedOrder, mark_price: f64) -> Fill {
        let is_maker = Self::is_maker(order);
        let slippage = if is_maker { 0.0 } else { self.cfg.slippage_bps as f64 / 10_000.0 };
        let fee_frac = if is_maker { self.cfg.maker_fee_bps } else { self.cfg.taker_fee_bps } as f64 / 10_000.0;

        match order.side {
            Side::Buy => {
                let effective_price = mark_price * (1.0 + slippage);
                let fee_quote = order.size_quote * fee_frac;
                let net_for_tokens = (order.size_quote - fee_quote).max(0.0);
                let qty = net_for_tokens / effective_price;
                Fill {
                    pair: order.pair.clone(),
                    side: Side::Buy,
                    qty,
                    price: effective_price,
                    quote_amount: order.size_quote, // matches what risk reserved
                    fee_quote,
                    funding_paid_quote: 0.0, // accrued separately, by holding duration - see engine.rs
                    slippage_bps: Some(self.cfg.slippage_bps),
                    market_type: order.pair_meta.market_type,
                    strategy: order.strategy.clone(),
                    reason: order.reason,
                    order_id: None,
                    dry_run: true,
                    ts: order.ts,
                }
            }
            Side::Sell => {
                let effective_price = mark_price * (1.0 - slippage);
                // order.size_quote for a sell is qty * mark_price from
                // risk's synthesized order (see risk_manager) - back out
                // qty, then compute real gross proceeds at the effective
                // price.
                let qty = if mark_price > 0.0 { order.size_quote / mark_price } else { 0.0 };
                let gross = qty * effective_price;
                let fee_quote = gross * fee_frac;
                let net_proceeds = (gross - fee_quote).max(0.0);
                Fill {
                    pair: order.pair.clone(),
                    side: Side::Sell,
                    qty,
                    price: effective_price,
                    quote_amount: net_proceeds,
                    fee_quote,
                    funding_paid_quote: 0.0,
                    slippage_bps: Some(self.cfg.slippage_bps),
                    market_type: order.pair_meta.market_type,
                    strategy: order.strategy.clone(),
                    reason: order.reason,
                    order_id: None,
                    dry_run: true,
                    ts: order.ts,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::{MarketType, Pair, PairMeta, RiskTier};

    fn order(side: Side, size_quote: f64, reason: OrderReason) -> ApprovedOrder {
        ApprovedOrder {
            side,
            pair: Pair::from("XBT/USD"),
            size_quote,
            max_slippage_bps: 100,
            reason,
            pair_meta: PairMeta { pair: Pair::from("XBT/USD"), market_type: MarketType::Spot, risk_tier: RiskTier::Spot, leverage: 1.0 },
            strategy: "test".into(),
            ts: 0,
        }
    }

    fn cfg() -> SimulatedExecutorConfig {
        SimulatedExecutorConfig { slippage_bps: 100, maker_fee_bps: 25, taker_fee_bps: 40 }
    }

    #[test]
    fn strategy_buy_fills_as_a_maker_with_no_slippage() {
        let exec = SimulatedExecutor::new(cfg());
        let fill = exec.fill(&order(Side::Buy, 10.0, OrderReason::Strategy), 1.0);
        assert_eq!(fill.price, 1.0, "maker fill has no slippage");
        assert_eq!(fill.quote_amount, 10.0);
        assert!(fill.qty > 0.0 && fill.qty < 10.0);
    }

    #[test]
    fn stop_loss_sell_fills_as_a_taker_with_slippage() {
        let exec = SimulatedExecutor::new(cfg());
        let fill = exec.fill(&order(Side::Sell, 10.0, OrderReason::StopLoss), 1.0);
        assert!(fill.price < 1.0, "taker sell should fill below mark price due to slippage");
    }

    #[test]
    fn maker_fee_is_cheaper_than_taker_fee() {
        let exec = SimulatedExecutor::new(cfg());
        let maker_fill = exec.fill(&order(Side::Sell, 10.0, OrderReason::Strategy), 1.0);
        let taker_fill = exec.fill(&order(Side::Sell, 10.0, OrderReason::StopLoss), 1.0);
        assert!(maker_fill.fee_quote < taker_fill.fee_quote);
    }

    #[test]
    fn higher_taker_fees_produce_lower_net_proceeds() {
        let mk = |taker_fee_bps: u16| {
            SimulatedExecutor::new(SimulatedExecutorConfig { slippage_bps: 0, maker_fee_bps: 0, taker_fee_bps })
        };
        let low_fee = mk(10).fill(&order(Side::Sell, 10.0, OrderReason::StopLoss), 1.0);
        let high_fee = mk(500).fill(&order(Side::Sell, 10.0, OrderReason::StopLoss), 1.0);
        assert!(high_fee.quote_amount < low_fee.quote_amount);
    }
}
