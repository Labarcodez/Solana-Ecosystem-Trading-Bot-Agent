//! Fills `ApprovedOrder`s against a modeled fee + slippage curve instead of
//! a real DEX. This is a documented simplification (see README): it models
//! network fee + a modeled Jito tip + a flat slippage %, not order-book
//! depth or Jupiter's real-time route price impact.

use bot_core::{ApprovedOrder, Fill, Side};

#[derive(Debug, Clone, Copy)]
pub struct SimulatedExecutorConfig {
    pub slippage_bps: u16,
    pub fee_bps: u16,
    pub jito_tip_lamports: u64,
}

pub struct SimulatedExecutor {
    cfg: SimulatedExecutorConfig,
}

impl SimulatedExecutor {
    pub fn new(cfg: SimulatedExecutorConfig) -> Self {
        Self { cfg }
    }

    fn jito_tip_sol(&self) -> f64 {
        self.cfg.jito_tip_lamports as f64 / 1_000_000_000.0
    }

    /// Fill `order` at `mark_price` (the current tick price), applying
    /// slippage against the trade direction and deducting the modeled fee +
    /// tip from the notional. `order.size_sol` is treated as the exact
    /// amount of capital `risk` already reserved (buys) or the exact gross
    /// notional being sold (sells) - fee and tip come out of that same
    /// amount, so `Fill.sol_amount` always matches what `RiskManager`
    /// expects to move in/out of `capital_sol`.
    pub fn fill(&self, order: &ApprovedOrder, mark_price: f64) -> Fill {
        let slippage = self.cfg.slippage_bps as f64 / 10_000.0;
        let fee_frac = self.cfg.fee_bps as f64 / 10_000.0;
        let tip_sol = self.jito_tip_sol();

        match order.side {
            Side::Buy => {
                let effective_price = mark_price * (1.0 + slippage);
                let fee_sol = order.size_sol * fee_frac;
                let net_for_tokens = (order.size_sol - fee_sol - tip_sol).max(0.0);
                let qty = net_for_tokens / effective_price;
                Fill {
                    mint: order.mint,
                    side: Side::Buy,
                    qty,
                    price: effective_price,
                    sol_amount: order.size_sol, // matches what risk reserved
                    fee_sol,
                    jito_tip_sol: tip_sol,
                    slippage_bps: Some(self.cfg.slippage_bps),
                    strategy: order.strategy.clone(),
                    reason: order.reason,
                    tx_signature: None,
                    bundle_id: None,
                    dry_run: true,
                    ts: order.ts,
                }
            }
            Side::Sell => {
                let effective_price = mark_price * (1.0 - slippage);
                // order.size_sol for a sell is qty * mark_price from risk's
                // synthesized order (see risk_manager) - back out qty, then
                // compute real gross proceeds at the effective price.
                let qty = if mark_price > 0.0 { order.size_sol / mark_price } else { 0.0 };
                let gross = qty * effective_price;
                let fee_sol = gross * fee_frac;
                let net_proceeds = (gross - fee_sol - tip_sol).max(0.0);
                Fill {
                    mint: order.mint,
                    side: Side::Sell,
                    qty,
                    price: effective_price,
                    sol_amount: net_proceeds,
                    fee_sol,
                    jito_tip_sol: tip_sol,
                    slippage_bps: Some(self.cfg.slippage_bps),
                    strategy: order.strategy.clone(),
                    reason: order.reason,
                    tx_signature: None,
                    bundle_id: None,
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
    use bot_core::{OrderReason, Pubkey, TokenMeta, TokenPhase, TrustTier};

    fn order(side: Side, size_sol: f64) -> ApprovedOrder {
        ApprovedOrder {
            side,
            mint: Pubkey::new_unique(),
            size_sol,
            max_slippage_bps: 100,
            reason: OrderReason::Strategy,
            token_meta: TokenMeta {
                mint: Pubkey::new_unique(),
                phase: TokenPhase::Migrated,
                trust_tier: TrustTier::Established,
                discovered_at: 0,
                source: "test".into(),
            },
            strategy: "test".into(),
            ts: 0,
        }
    }

    #[test]
    fn buy_fill_applies_positive_slippage_and_fee() {
        let exec = SimulatedExecutor::new(SimulatedExecutorConfig {
            slippage_bps: 100, // 1%
            fee_bps: 30,       // 0.3%
            jito_tip_lamports: 10_000,
        });
        let fill = exec.fill(&order(Side::Buy, 10.0), 1.0);
        assert!(fill.price > 1.0, "buy should fill above mark price due to slippage");
        assert_eq!(fill.sol_amount, 10.0);
        assert!(fill.qty > 0.0 && fill.qty < 10.0);
    }

    #[test]
    fn sell_fill_applies_negative_slippage_and_fee() {
        let exec = SimulatedExecutor::new(SimulatedExecutorConfig {
            slippage_bps: 100,
            fee_bps: 30,
            jito_tip_lamports: 10_000,
        });
        let fill = exec.fill(&order(Side::Sell, 10.0), 1.0);
        assert!(fill.price < 1.0, "sell should fill below mark price due to slippage");
        assert!(fill.sol_amount < 10.0, "net proceeds should be less than gross due to fee+tip");
    }

    #[test]
    fn higher_fees_produce_lower_net_proceeds() {
        let mk = |fee_bps: u16| {
            SimulatedExecutor::new(SimulatedExecutorConfig { slippage_bps: 0, fee_bps, jito_tip_lamports: 0 })
        };
        let low_fee = mk(10).fill(&order(Side::Sell, 10.0), 1.0);
        let high_fee = mk(500).fill(&order(Side::Sell, 10.0), 1.0);
        assert!(high_fee.sol_amount < low_fee.sol_amount);
    }
}
