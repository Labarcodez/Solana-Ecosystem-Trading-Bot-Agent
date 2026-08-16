use std::collections::HashMap;

use bot_core::{
    AppEvent, ApprovedOrder, Fill, OrderReason, PriceTick, Pubkey, Side, Signal, TokenMeta,
    TrustTier,
};
use chrono::NaiveDate;

use crate::circuit_breaker::CircuitBreaker;
use crate::config::{RiskConfig, TierConfig};
use crate::position::OpenPosition;

#[derive(Debug, Clone)]
pub enum RiskDecision {
    Approved(ApprovedOrder),
    Rejected(String),
}

/// What a not-yet-filled buy needs remembered so `on_fill` can open the
/// position with the *same* trust tier, strategy, and SL/TP that were
/// decided at approval time - not a fallback reconstruction.
#[derive(Debug, Clone)]
struct PendingBuy {
    token_meta: TokenMeta,
    strategy: String,
    stop_loss_pct: f64,
    take_profit_pct: Option<f64>,
}

/// The sole veto point between a strategy signal and the executor. Also
/// subscribes conceptually to every price tick (via `on_price_tick`) so
/// stop-loss/take-profit fire regardless of whether the strategy emits a
/// signal that tick, and owns the daily-loss circuit breaker.
pub struct RiskManager {
    cfg: RiskConfig,
    tiers: HashMap<TrustTier, TierConfig>,
    default_slippage_bps: u16,

    capital_sol: f64,
    starting_daily_capital_sol: f64,
    daily_realized_pnl_sol: f64,
    day: Option<NaiveDate>,

    open_positions: HashMap<Pubkey, OpenPosition>,
    pending_buys: HashMap<Pubkey, PendingBuy>,
    last_prices: HashMap<Pubkey, f64>,

    breaker: CircuitBreaker,
    events: Vec<AppEvent>,
}

impl RiskManager {
    pub fn new(
        cfg: RiskConfig,
        tiers: HashMap<TrustTier, TierConfig>,
        default_slippage_bps: u16,
        starting_capital_sol: f64,
    ) -> Self {
        let breaker = CircuitBreaker::new(cfg.daily_loss_limit_sol, cfg.daily_loss_limit_pct);
        Self {
            cfg,
            tiers,
            default_slippage_bps,
            capital_sol: starting_capital_sol,
            starting_daily_capital_sol: starting_capital_sol,
            daily_realized_pnl_sol: 0.0,
            day: None,
            open_positions: HashMap::new(),
            pending_buys: HashMap::new(),
            last_prices: HashMap::new(),
            breaker,
            events: Vec::new(),
        }
    }

    pub fn capital_sol(&self) -> f64 {
        self.capital_sol
    }

    pub fn open_position_count(&self) -> usize {
        self.open_positions.len()
    }

    pub fn is_breaker_tripped(&self) -> bool {
        self.breaker.is_tripped()
    }

    pub fn open_positions(&self) -> impl Iterator<Item = &OpenPosition> {
        self.open_positions.values()
    }

    pub fn daily_pnl_sol(&self) -> f64 {
        self.daily_realized_pnl_sol + self.unrealized_pnl_sol()
    }

    /// Realized PnL accumulated so far today (resets on UTC day rollover).
    pub fn realized_pnl_sol(&self) -> f64 {
        self.daily_realized_pnl_sol
    }

    /// Mark-to-market unrealized PnL across all open positions.
    pub fn total_unrealized_pnl_sol(&self) -> f64 {
        self.unrealized_pnl_sol()
    }

    /// Total mark-to-market equity: free capital plus the current value of
    /// every open position at its last-seen price. Used for the TUI's
    /// equity display and the backtester's equity curve.
    pub fn equity_sol(&self) -> f64 {
        let positions_value: f64 = self
            .open_positions
            .values()
            .map(|p| {
                let price = self
                    .last_prices
                    .get(&p.token_meta.mint)
                    .copied()
                    .unwrap_or(p.entry_price);
                p.qty * price
            })
            .sum();
        self.capital_sol + positions_value
    }

    /// Drain events accumulated since the last call (circuit breaker
    /// trip/reset notifications). The caller forwards these onto the shared
    /// event bus.
    pub fn drain_events(&mut self) -> Vec<AppEvent> {
        std::mem::take(&mut self.events)
    }

    fn tier_cfg(&self, tier: TrustTier) -> Option<TierConfig> {
        self.tiers.get(&tier).copied()
    }

    fn effective_stop_loss_pct(&self, tier: TrustTier) -> f64 {
        match self.tier_cfg(tier) {
            Some(t) if t.mandatory_stop_loss_pct > 0.0 => t.mandatory_stop_loss_pct,
            _ => self.cfg.default_stop_loss_pct,
        }
    }

    fn effective_take_profit_pct(&self, tier: TrustTier) -> Option<f64> {
        match self.tier_cfg(tier) {
            Some(t) if t.allow_take_profit_override => Some(self.cfg.default_take_profit_pct),
            Some(_) => None, // tier forbids take-profit overrides (e.g. bonding_curve)
            None => Some(self.cfg.default_take_profit_pct),
        }
    }

    fn maybe_roll_day(&mut self, ts: i64) {
        let Some(date) = chrono::DateTime::from_timestamp(ts, 0).map(|dt| dt.date_naive()) else {
            return;
        };
        if Some(date) != self.day {
            self.day = Some(date);
            self.daily_realized_pnl_sol = 0.0;
            self.starting_daily_capital_sol = self.capital_sol;
            if self.breaker.is_tripped() {
                self.breaker.reset();
                self.events.push(AppEvent::CircuitBreakerReset { ts });
            }
        }
    }

    fn unrealized_pnl_sol(&self) -> f64 {
        self.open_positions
            .values()
            .map(|p| {
                let price = self
                    .last_prices
                    .get(&p.token_meta.mint)
                    .copied()
                    .unwrap_or(p.entry_price);
                p.unrealized_pnl_sol(price)
            })
            .sum()
    }

    /// Called on every price tick for every watched mint - not just ones
    /// with open positions - so SL/TP checks never depend on the strategy
    /// having produced a signal this tick.
    pub fn on_price_tick(&mut self, tick: &PriceTick) -> Vec<ApprovedOrder> {
        self.maybe_roll_day(tick.ts);
        self.last_prices.insert(tick.mint, tick.price);

        let mut orders = Vec::new();
        if let Some(pos) = self.open_positions.get_mut(&tick.mint) {
            if !pos.pending_exit {
                let pnl_pct = pos.pnl_pct(tick.price);
                let reason = if pnl_pct <= -pos.stop_loss_pct {
                    Some(OrderReason::StopLoss)
                } else if pos.take_profit_pct.map(|tp| pnl_pct >= tp).unwrap_or(false) {
                    Some(OrderReason::TakeProfit)
                } else {
                    None
                };
                if let Some(reason) = reason {
                    pos.pending_exit = true;
                    orders.push(ApprovedOrder {
                        side: Side::Sell,
                        mint: pos.token_meta.mint,
                        size_sol: pos.qty * tick.price,
                        max_slippage_bps: self.default_slippage_bps,
                        reason,
                        token_meta: pos.token_meta.clone(),
                        strategy: pos.strategy.clone(),
                        ts: tick.ts,
                    });
                }
            }
        }

        if let Some(reason) =
            self.breaker
                .check(self.daily_pnl_sol(), self.starting_daily_capital_sol, tick.ts)
        {
            self.events.push(AppEvent::CircuitBreakerTripped { reason, ts: tick.ts });
        }

        orders
    }

    /// Evaluate a strategy-generated signal. This is the single veto point
    /// between a strategy and the executor - nothing reaches `execution`
    /// without going through here first.
    pub fn evaluate_signal(
        &mut self,
        signal: &Signal,
        token_meta: &TokenMeta,
        current_price: f64,
    ) -> RiskDecision {
        match signal.side {
            Side::Buy => self.evaluate_buy(signal, token_meta),
            Side::Sell => self.evaluate_sell(signal, token_meta, current_price),
        }
    }

    fn evaluate_buy(&mut self, signal: &Signal, token_meta: &TokenMeta) -> RiskDecision {
        if self.breaker.is_tripped() {
            return RiskDecision::Rejected("circuit breaker tripped: new entries blocked".into());
        }
        if self.open_positions.contains_key(&signal.mint) || self.pending_buys.contains_key(&signal.mint) {
            return RiskDecision::Rejected("position already open or pending for this mint".into());
        }
        if self.open_positions.len() >= self.cfg.max_open_positions {
            return RiskDecision::Rejected(format!(
                "max_open_positions ({}) reached",
                self.cfg.max_open_positions
            ));
        }
        let Some(tier_cfg) = self.tier_cfg(token_meta.trust_tier) else {
            return RiskDecision::Rejected("no risk-tier configuration for this token's trust tier".into());
        };

        let strength = signal.strength.clamp(0.0, 1.0);
        let by_pct = self.capital_sol * (tier_cfg.risk_pct_of_capital / 100.0) * strength;
        let size_sol = by_pct.min(tier_cfg.max_position_sol);

        if size_sol <= 0.0 {
            return RiskDecision::Rejected("computed position size is zero".into());
        }
        if size_sol > self.capital_sol {
            return RiskDecision::Rejected("insufficient available capital".into());
        }

        self.pending_buys.insert(
            signal.mint,
            PendingBuy {
                token_meta: token_meta.clone(),
                strategy: signal.strategy.clone(),
                stop_loss_pct: self.effective_stop_loss_pct(token_meta.trust_tier),
                take_profit_pct: self.effective_take_profit_pct(token_meta.trust_tier),
            },
        );
        self.capital_sol -= size_sol; // reserved until fill/rejection reconciles it

        RiskDecision::Approved(ApprovedOrder {
            side: Side::Buy,
            mint: signal.mint,
            size_sol,
            max_slippage_bps: self.default_slippage_bps,
            reason: OrderReason::Strategy,
            token_meta: token_meta.clone(),
            strategy: signal.strategy.clone(),
            ts: signal.ts,
        })
    }

    fn evaluate_sell(&mut self, signal: &Signal, token_meta: &TokenMeta, current_price: f64) -> RiskDecision {
        let Some(pos) = self.open_positions.get_mut(&signal.mint) else {
            return RiskDecision::Rejected("no open position to sell".into());
        };
        if pos.pending_exit {
            return RiskDecision::Rejected("an exit for this position is already in flight".into());
        }
        pos.pending_exit = true;
        RiskDecision::Approved(ApprovedOrder {
            side: Side::Sell,
            mint: signal.mint,
            size_sol: pos.qty * current_price,
            max_slippage_bps: self.default_slippage_bps,
            reason: OrderReason::Strategy,
            token_meta: token_meta.clone(),
            strategy: signal.strategy.clone(),
            ts: signal.ts,
        })
    }

    /// Reject a pending or open order that failed at execution time (e.g.
    /// the swap route disappeared, a safety re-check failed). Releases any
    /// capital reserved for a pending buy, or clears `pending_exit` so a
    /// sell can be retried on a future tick/signal.
    pub fn on_order_rejected(&mut self, mint: &Pubkey, side: Side, size_sol_reserved: Option<f64>) {
        match side {
            Side::Buy => {
                self.pending_buys.remove(mint);
                if let Some(size) = size_sol_reserved {
                    self.capital_sol += size;
                }
            }
            Side::Sell => {
                if let Some(pos) = self.open_positions.get_mut(mint) {
                    pos.pending_exit = false;
                }
            }
        }
    }

    /// Reconcile a fill: opens a new position on a Buy fill (using the
    /// tier/SL/TP decided at approval time), closes and realizes PnL on a
    /// Sell fill, and releases capital reserved at approval time.
    pub fn on_fill(&mut self, fill: &Fill) {
        match fill.side {
            Side::Buy => {
                let pending = self.pending_buys.remove(&fill.mint);
                let (token_meta, strategy, stop_loss_pct, take_profit_pct) = match pending {
                    Some(p) => (p.token_meta, p.strategy, p.stop_loss_pct, p.take_profit_pct),
                    None => {
                        // No matching approval on record (e.g. a test driving
                        // on_fill directly). Fall back to a conservative
                        // Established-tier default rather than panicking.
                        (
                            TokenMeta {
                                mint: fill.mint,
                                phase: bot_core::TokenPhase::Migrated,
                                trust_tier: TrustTier::Established,
                                discovered_at: fill.ts,
                                source: "unmatched_fill".into(),
                            },
                            fill.strategy.clone(),
                            self.cfg.default_stop_loss_pct,
                            Some(self.cfg.default_take_profit_pct),
                        )
                    }
                };
                self.open_positions.insert(
                    fill.mint,
                    OpenPosition {
                        token_meta,
                        entry_price: fill.price,
                        qty: fill.qty,
                        size_sol: fill.sol_amount,
                        strategy,
                        opened_ts: fill.ts,
                        stop_loss_pct,
                        take_profit_pct,
                        pending_exit: false,
                    },
                );
            }
            Side::Sell => {
                if let Some(pos) = self.open_positions.remove(&fill.mint) {
                    let realized =
                        (fill.price - pos.entry_price) * pos.qty - fill.fee_sol - fill.jito_tip_sol;
                    self.daily_realized_pnl_sol += realized;
                    self.capital_sol += fill.sol_amount;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_tiers;
    use bot_core::{OrderReason, TokenPhase};

    fn meta(mint: Pubkey, tier: TrustTier) -> TokenMeta {
        TokenMeta {
            mint,
            phase: TokenPhase::Migrated,
            trust_tier: tier,
            discovered_at: 0,
            source: "test".into(),
        }
    }

    fn signal(side: Side, mint: Pubkey, strength: f64, ts: i64) -> Signal {
        Signal { side, mint, strength, reason: "test".into(), strategy: "test".into(), ts }
    }

    fn rm(starting_capital: f64) -> RiskManager {
        RiskManager::new(RiskConfig::default(), default_tiers(), 100, starting_capital)
    }

    #[test]
    fn buy_sizing_respects_tier_cap_and_strength() {
        let mut r = rm(100.0);
        let mint = Pubkey::new_unique();
        let m = meta(mint, TrustTier::BondingCurve); // max_position_sol = 0.05
        let sig = signal(Side::Buy, mint, 1.0, 0);
        match r.evaluate_signal(&sig, &m, 1.0) {
            RiskDecision::Approved(order) => {
                assert!(order.size_sol <= 0.05 + 1e-9);
            }
            RiskDecision::Rejected(reason) => panic!("expected approval, got: {reason}"),
        }
    }

    #[test]
    fn rejects_buy_when_max_open_positions_reached() {
        let cfg = RiskConfig { max_open_positions: 1, ..RiskConfig::default() };
        let mut r = RiskManager::new(cfg, default_tiers(), 100, 100.0);
        let mint1 = Pubkey::new_unique();
        let mint2 = Pubkey::new_unique();
        let m1 = meta(mint1, TrustTier::Established);
        let m2 = meta(mint2, TrustTier::Established);

        let approved = r.evaluate_signal(&signal(Side::Buy, mint1, 1.0, 0), &m1, 1.0);
        assert!(matches!(approved, RiskDecision::Approved(_)));
        // Simulate the fill landing so it counts as an open position.
        if let RiskDecision::Approved(order) = approved {
            r.on_fill(&Fill {
                mint: mint1, side: Side::Buy, qty: 10.0, price: 1.0,
                sol_amount: order.size_sol, fee_sol: 0.0, jito_tip_sol: 0.0,
                slippage_bps: None, strategy: "test".into(), reason: OrderReason::Strategy,
                tx_signature: None, bundle_id: None, dry_run: true, ts: 0,
            });
        }

        let rejected = r.evaluate_signal(&signal(Side::Buy, mint2, 1.0, 1), &m2, 1.0);
        assert!(matches!(rejected, RiskDecision::Rejected(_)));
    }

    #[test]
    fn sell_without_open_position_is_rejected() {
        let mut r = rm(100.0);
        let mint = Pubkey::new_unique();
        let m = meta(mint, TrustTier::Established);
        let rejected = r.evaluate_signal(&signal(Side::Sell, mint, 1.0, 0), &m, 1.0);
        assert!(matches!(rejected, RiskDecision::Rejected(_)));
    }

    #[test]
    fn stop_loss_fires_on_price_tick_without_a_strategy_signal() {
        let mut r = rm(100.0);
        let mint = Pubkey::new_unique();
        r.on_fill(&Fill {
            mint, side: Side::Buy, qty: 10.0, price: 1.0, sol_amount: 10.0,
            fee_sol: 0.0, jito_tip_sol: 0.0, slippage_bps: None,
            strategy: "test".into(), reason: OrderReason::Strategy,
            tx_signature: None, bundle_id: None, dry_run: true, ts: 0,
        });
        // Established tier default stop loss is 8% (mandatory_stop_loss_pct = 0.0 -> use default).
        let orders = r.on_price_tick(&PriceTick { mint, price: 0.90, ts: 1 });
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].reason, OrderReason::StopLoss);
        assert_eq!(orders[0].side, Side::Sell);
    }

    #[test]
    fn stop_loss_does_not_fire_twice_while_exit_pending() {
        let mut r = rm(100.0);
        let mint = Pubkey::new_unique();
        r.on_fill(&Fill {
            mint, side: Side::Buy, qty: 10.0, price: 1.0, sol_amount: 10.0,
            fee_sol: 0.0, jito_tip_sol: 0.0, slippage_bps: None,
            strategy: "test".into(), reason: OrderReason::Strategy,
            tx_signature: None, bundle_id: None, dry_run: true, ts: 0,
        });
        let first = r.on_price_tick(&PriceTick { mint, price: 0.90, ts: 1 });
        assert_eq!(first.len(), 1);
        let second = r.on_price_tick(&PriceTick { mint, price: 0.80, ts: 2 });
        assert!(second.is_empty(), "should not re-fire while an exit is pending");
    }

    #[test]
    fn circuit_breaker_blocks_new_buys_but_not_stop_loss_sells() {
        let cfg = RiskConfig {
            daily_loss_limit_sol: 0.5,
            daily_loss_limit_pct: 1000.0, // effectively disable the pct path
            ..RiskConfig::default()
        };
        let mut r = RiskManager::new(cfg, default_tiers(), 100, 100.0);
        let mint = Pubkey::new_unique();

        r.on_fill(&Fill {
            mint, side: Side::Buy, qty: 100.0, price: 1.0, sol_amount: 10.0,
            fee_sol: 0.0, jito_tip_sol: 0.0, slippage_bps: None,
            strategy: "test".into(), reason: OrderReason::Strategy,
            tx_signature: None, bundle_id: None, dry_run: true, ts: 0,
        });

        // A big enough drop trips the breaker AND fires the stop loss in the
        // same tick.
        let orders = r.on_price_tick(&PriceTick { mint, price: 0.5, ts: 1 });
        assert_eq!(orders.len(), 1, "protective sell should still fire");
        assert!(r.is_breaker_tripped());

        // A brand new buy on a different mint must now be rejected.
        let mint2 = Pubkey::new_unique();
        let m2 = meta(mint2, TrustTier::Established);
        let rejected = r.evaluate_signal(&signal(Side::Buy, mint2, 1.0, 2), &m2, 1.0);
        assert!(matches!(rejected, RiskDecision::Rejected(_)));
    }

    #[test]
    fn day_rollover_resets_daily_pnl_and_breaker() {
        let cfg = RiskConfig {
            daily_loss_limit_sol: 0.1,
            daily_loss_limit_pct: 1000.0,
            ..RiskConfig::default()
        };
        let mut r = RiskManager::new(cfg, default_tiers(), 100, 100.0);
        let mint = Pubkey::new_unique();

        r.on_fill(&Fill {
            mint, side: Side::Buy, qty: 100.0, price: 1.0, sol_amount: 10.0,
            fee_sol: 0.0, jito_tip_sol: 0.0, slippage_bps: None,
            strategy: "test".into(), reason: OrderReason::Strategy,
            tx_signature: None, bundle_id: None, dry_run: true, ts: 0,
        });
        let exits = r.on_price_tick(&PriceTick { mint, price: 0.5, ts: 1 });
        assert!(r.is_breaker_tripped());
        // Simulate the protective sell actually landing, closing the
        // position - otherwise it would still be open and underwater on the
        // next day, which should (correctly) keep tripping the breaker.
        assert_eq!(exits.len(), 1);
        r.on_fill(&Fill {
            mint, side: Side::Sell, qty: 100.0, price: 0.5, sol_amount: 50.0,
            fee_sol: 0.0, jito_tip_sol: 0.0, slippage_bps: None,
            strategy: "test".into(), reason: OrderReason::StopLoss,
            tx_signature: None, bundle_id: None, dry_run: true, ts: 1,
        });

        // Jump to the next UTC day (+ 90000s ~ 25h from epoch) - should
        // reset the breaker and daily PnL tracking now that the position is
        // actually closed.
        r.on_price_tick(&PriceTick { mint, price: 0.5, ts: 90_000 });
        assert!(!r.is_breaker_tripped());
    }

    #[test]
    fn tighter_daily_loss_limit_trips_sooner() {
        // Same loss path, different configured limits -> different trip
        // outcomes. Confirms the breaker actually reads its config.
        let mint = Pubkey::new_unique();
        let make = |limit_sol: f64| {
            let cfg = RiskConfig {
                daily_loss_limit_sol: limit_sol,
                daily_loss_limit_pct: 1000.0,
                ..RiskConfig::default()
            };
            RiskManager::new(cfg, default_tiers(), 100, 100.0)
        };
        let mut loose = make(50.0);
        let mut strict = make(0.01);
        for r in [&mut loose, &mut strict] {
            r.on_fill(&Fill {
                mint, side: Side::Buy, qty: 100.0, price: 1.0, sol_amount: 10.0,
                fee_sol: 0.0, jito_tip_sol: 0.0, slippage_bps: None,
                strategy: "test".into(), reason: OrderReason::Strategy,
                tx_signature: None, bundle_id: None, dry_run: true, ts: 0,
            });
            r.on_price_tick(&PriceTick { mint, price: 0.95, ts: 1 });
        }
        assert!(!loose.is_breaker_tripped());
        assert!(strict.is_breaker_tripped());
    }
}
