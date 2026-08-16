//! Dashboard state. Deliberately separated from rendering (`ui.rs`) so it's
//! unit-testable without a terminal: `App` only ever gets fed `PriceTick`s,
//! `AppEvent`s, and periodic `Snapshot`s (the same `watch`-channel snapshot
//! the plan calls for), same as the real dashboard task does.

use std::collections::{HashMap, VecDeque};

use bot_core::{AppEvent, PriceTick, Pubkey, Side, Snapshot};

const MAX_PRICE_HISTORY: usize = 300;
const MAX_EVENTS: usize = 200;

#[derive(Debug, Clone)]
pub struct OpenPositionView {
    pub mint: Pubkey,
    pub entry_price: f64,
    pub qty: f64,
    pub strategy: String,
    pub opened_ts: i64,
}

pub struct App {
    pub mode: String,
    pub strategy: String,
    pub paused: bool,

    pub last_price: HashMap<Pubkey, f64>,
    /// Price history for the first mint seen - enough for a single-asset
    /// dry-run/mock demo; a live multi-asset run would key this per-mint,
    /// noted as a natural extension rather than implemented here.
    pub primary_mint: Option<Pubkey>,
    pub price_history: VecDeque<(i64, f64)>,

    pub positions: HashMap<Pubkey, OpenPositionView>,
    pub trade_count: usize,

    /// Authoritative PnL/equity figures, fed from RiskManager via a
    /// `watch::Receiver<Snapshot>` - not recomputed independently here, to
    /// avoid the dashboard's numbers ever drifting from risk's.
    pub snapshot: Snapshot,

    pub events: VecDeque<String>,
}

impl App {
    pub fn new(mode: impl Into<String>, strategy: impl Into<String>) -> Self {
        Self {
            mode: mode.into(),
            strategy: strategy.into(),
            paused: false,
            last_price: HashMap::new(),
            primary_mint: None,
            price_history: VecDeque::with_capacity(MAX_PRICE_HISTORY),
            positions: HashMap::new(),
            trade_count: 0,
            snapshot: Snapshot::default(),
            events: VecDeque::with_capacity(MAX_EVENTS),
        }
    }

    pub fn on_price_tick(&mut self, tick: &PriceTick) {
        self.last_price.insert(tick.mint, tick.price);
        if self.primary_mint.is_none() {
            self.primary_mint = Some(tick.mint);
        }
        if self.primary_mint == Some(tick.mint) {
            self.price_history.push_back((tick.ts, tick.price));
            if self.price_history.len() > MAX_PRICE_HISTORY {
                self.price_history.pop_front();
            }
        }
    }

    pub fn on_snapshot(&mut self, snapshot: Snapshot) {
        self.snapshot = snapshot;
    }

    pub fn on_app_event(&mut self, event: &AppEvent) {
        match event {
            AppEvent::Fill(fill) => {
                self.trade_count += 1;
                match fill.side {
                    Side::Buy => {
                        self.positions.insert(
                            fill.mint,
                            OpenPositionView {
                                mint: fill.mint,
                                entry_price: fill.price,
                                qty: fill.qty,
                                strategy: fill.strategy.clone(),
                                opened_ts: fill.ts,
                            },
                        );
                    }
                    Side::Sell => {
                        self.positions.remove(&fill.mint);
                    }
                }
                self.push_event(format!(
                    "{:?} {:.4} {} @ {:.6} SOL{}",
                    fill.side,
                    fill.qty,
                    short_mint(&fill.mint),
                    fill.price,
                    if fill.dry_run { " [DRY-RUN]" } else { "" }
                ));
            }
            AppEvent::OrderRejected { mint, reason } => {
                self.push_event(format!("rejected {}: {reason}", short_mint(mint)));
            }
            AppEvent::CircuitBreakerTripped { reason, .. } => {
                self.push_event(format!("CIRCUIT BREAKER TRIPPED: {reason}"));
            }
            AppEvent::CircuitBreakerReset { .. } => {
                self.push_event("circuit breaker reset".to_string());
            }
            AppEvent::TokenDiscovered(meta) => {
                self.push_event(format!("discovered {} ({:?})", short_mint(&meta.mint), meta.phase));
            }
            AppEvent::TokenRejectedBySafety { mint, reasons } => {
                self.push_event(format!("safety rejected {}: {}", short_mint(mint), reasons.join("; ")));
            }
            AppEvent::Log { message, .. } => self.push_event(message.clone()),
        }
    }

    pub fn toggle_pause(&mut self) {
        self.paused = !self.paused;
    }

    fn push_event(&mut self, s: String) {
        self.events.push_back(s);
        if self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
    }
}

pub fn short_mint(mint: &Pubkey) -> String {
    let s = mint.to_string();
    if s.len() > 8 {
        format!("{}..{}", &s[..4], &s[s.len() - 4..])
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::{OrderReason, TokenMeta, TokenPhase, TrustTier};

    fn tick(mint: Pubkey, price: f64, ts: i64) -> PriceTick {
        PriceTick { mint, price, ts }
    }

    #[test]
    fn tracks_price_history_for_the_primary_mint_only() {
        let mut app = App::new("dry_run", "momentum");
        let mint1 = Pubkey::new_unique();
        let mint2 = Pubkey::new_unique();
        app.on_price_tick(&tick(mint1, 1.0, 0));
        app.on_price_tick(&tick(mint2, 2.0, 1)); // a different mint - ignored for history
        app.on_price_tick(&tick(mint1, 1.5, 2));
        assert_eq!(app.price_history.len(), 2);
        assert_eq!(app.last_price.get(&mint2), Some(&2.0));
    }

    #[test]
    fn buy_fill_opens_a_position_sell_fill_closes_it() {
        let mut app = App::new("dry_run", "momentum");
        let mint = Pubkey::new_unique();
        let buy = bot_core::Fill {
            mint, side: Side::Buy, qty: 10.0, price: 1.0, sol_amount: 10.0,
            fee_sol: 0.0, jito_tip_sol: 0.0, slippage_bps: None,
            strategy: "momentum".into(), reason: OrderReason::Strategy,
            tx_signature: None, bundle_id: None, dry_run: true, ts: 0,
        };
        app.on_app_event(&AppEvent::Fill(buy));
        assert_eq!(app.positions.len(), 1);
        assert_eq!(app.trade_count, 1);

        let sell = bot_core::Fill {
            mint, side: Side::Sell, qty: 10.0, price: 1.2, sol_amount: 12.0,
            fee_sol: 0.0, jito_tip_sol: 0.0, slippage_bps: None,
            strategy: "momentum".into(), reason: OrderReason::Strategy,
            tx_signature: None, bundle_id: None, dry_run: true, ts: 1,
        };
        app.on_app_event(&AppEvent::Fill(sell));
        assert!(app.positions.is_empty());
        assert_eq!(app.trade_count, 2);
    }

    #[test]
    fn circuit_breaker_events_are_logged() {
        let mut app = App::new("dry_run", "momentum");
        app.on_app_event(&AppEvent::CircuitBreakerTripped { reason: "daily loss".into(), ts: 0 });
        assert!(app.events.back().unwrap().contains("TRIPPED"));
        app.on_app_event(&AppEvent::CircuitBreakerReset { ts: 1 });
        assert!(app.events.back().unwrap().contains("reset"));
    }

    #[test]
    fn snapshot_updates_are_authoritative_not_recomputed() {
        let mut app = App::new("dry_run", "momentum");
        let snap = Snapshot {
            equity_sol: 42.0,
            realized_pnl_sol: 1.5,
            unrealized_pnl_sol: -0.5,
            daily_pnl_sol: 1.0,
            open_positions: 2,
            circuit_breaker_tripped: true,
        };
        app.on_snapshot(snap.clone());
        assert_eq!(app.snapshot.equity_sol, 42.0);
        assert!(app.snapshot.circuit_breaker_tripped);
    }

    #[test]
    fn event_log_is_capped() {
        let mut app = App::new("dry_run", "momentum");
        for i in 0..(MAX_EVENTS + 50) {
            app.on_app_event(&AppEvent::Log {
                level: bot_core::LogLevel::Info,
                message: format!("event {i}"),
                ts: i as i64,
            });
        }
        assert_eq!(app.events.len(), MAX_EVENTS);
        assert!(app.events.back().unwrap().contains(&(MAX_EVENTS + 49).to_string()));
    }

    #[test]
    fn discovery_and_safety_events_use_token_meta() {
        let mut app = App::new("dry_run", "momentum");
        let meta = TokenMeta {
            mint: Pubkey::new_unique(),
            phase: TokenPhase::BondingCurve,
            trust_tier: TrustTier::BondingCurve,
            discovered_at: 0,
            source: "pumpfun_create".into(),
        };
        app.on_app_event(&AppEvent::TokenDiscovered(meta));
        assert!(app.events.back().unwrap().contains("discovered"));
    }
}
