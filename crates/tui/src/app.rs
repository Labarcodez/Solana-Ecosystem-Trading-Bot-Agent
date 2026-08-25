//! Dashboard state. Deliberately separated from rendering (`ui.rs`) so it's
//! unit-testable without a terminal: `App` only ever gets fed `PriceTick`s,
//! `AppEvent`s, and periodic `Snapshot`s (the same `watch`-channel snapshot
//! the plan calls for), same as the real dashboard task does.

use std::collections::{HashMap, VecDeque};

use bot_core::{AppEvent, MarketType, Pair, PriceTick, Side, Snapshot};

const MAX_PRICE_HISTORY: usize = 300;
const MAX_EVENTS: usize = 200;

#[derive(Debug, Clone)]
pub struct OpenPositionView {
    pub pair: Pair,
    pub market_type: MarketType,
    pub entry_price: f64,
    pub qty: f64,
    pub strategy: String,
    pub opened_ts: i64,
}

pub struct App {
    pub mode: String,
    pub strategy: String,
    pub paused: bool,

    pub last_price: HashMap<Pair, f64>,
    /// Price history for the first pair seen - enough for a single-asset
    /// dry-run/mock demo; a live multi-pair run would key this per-pair,
    /// noted as a natural extension rather than implemented here.
    pub primary_pair: Option<Pair>,
    pub price_history: VecDeque<(i64, f64)>,

    pub positions: HashMap<Pair, OpenPositionView>,
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
            primary_pair: None,
            price_history: VecDeque::with_capacity(MAX_PRICE_HISTORY),
            positions: HashMap::new(),
            trade_count: 0,
            snapshot: Snapshot::default(),
            events: VecDeque::with_capacity(MAX_EVENTS),
        }
    }

    pub fn on_price_tick(&mut self, tick: &PriceTick) {
        self.last_price.insert(tick.pair.clone(), tick.price);
        if self.primary_pair.is_none() {
            self.primary_pair = Some(tick.pair.clone());
        }
        if self.primary_pair.as_ref() == Some(&tick.pair) {
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
                            fill.pair.clone(),
                            OpenPositionView {
                                pair: fill.pair.clone(),
                                market_type: fill.market_type,
                                entry_price: fill.price,
                                qty: fill.qty,
                                strategy: fill.strategy.clone(),
                                opened_ts: fill.ts,
                            },
                        );
                    }
                    Side::Sell => {
                        self.positions.remove(&fill.pair);
                    }
                }
                self.push_event(format!(
                    "{:?} {:.4} {} @ {:.6}{}",
                    fill.side,
                    fill.qty,
                    fill.pair,
                    fill.price,
                    if fill.dry_run { " [DRY-RUN]" } else { "" }
                ));
            }
            AppEvent::OrderRejected { pair, reason } => {
                self.push_event(format!("rejected {pair}: {reason}"));
            }
            AppEvent::CircuitBreakerTripped { reason, .. } => {
                self.push_event(format!("CIRCUIT BREAKER TRIPPED: {reason}"));
            }
            AppEvent::CircuitBreakerReset { .. } => {
                self.push_event("circuit breaker reset".to_string());
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

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::OrderReason;

    fn tick(pair: Pair, price: f64, ts: i64) -> PriceTick {
        PriceTick { pair, price, funding_rate: None, ts }
    }

    #[test]
    fn tracks_price_history_for_the_primary_pair_only() {
        let mut app = App::new("dry_run", "momentum");
        let pair1 = Pair::from("XBT/USD");
        let pair2 = Pair::from("ETH/USD");
        app.on_price_tick(&tick(pair1.clone(), 1.0, 0));
        app.on_price_tick(&tick(pair2.clone(), 2.0, 1)); // a different pair - ignored for history
        app.on_price_tick(&tick(pair1, 1.5, 2));
        assert_eq!(app.price_history.len(), 2);
        assert_eq!(app.last_price.get(&pair2), Some(&2.0));
    }

    #[test]
    fn buy_fill_opens_a_position_sell_fill_closes_it() {
        let mut app = App::new("dry_run", "momentum");
        let pair = Pair::from("XBT/USD");
        let buy = bot_core::Fill {
            pair: pair.clone(), side: Side::Buy, qty: 10.0, price: 1.0, quote_amount: 10.0,
            fee_quote: 0.0, funding_paid_quote: 0.0, slippage_bps: None, market_type: MarketType::Spot,
            strategy: "momentum".into(), reason: OrderReason::Strategy, order_id: None, dry_run: true, ts: 0,
        };
        app.on_app_event(&AppEvent::Fill(buy));
        assert_eq!(app.positions.len(), 1);
        assert_eq!(app.trade_count, 1);

        let sell = bot_core::Fill {
            pair: pair.clone(), side: Side::Sell, qty: 10.0, price: 1.2, quote_amount: 12.0,
            fee_quote: 0.0, funding_paid_quote: 0.0, slippage_bps: None, market_type: MarketType::Spot,
            strategy: "momentum".into(), reason: OrderReason::Strategy, order_id: None, dry_run: true, ts: 1,
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
            equity_quote: 42.0,
            realized_pnl_quote: 1.5,
            unrealized_pnl_quote: -0.5,
            daily_pnl_quote: 1.0,
            daily_funding_quote: 0.0,
            open_positions: 2,
            circuit_breaker_tripped: true,
        };
        app.on_snapshot(snap.clone());
        assert_eq!(app.snapshot.equity_quote, 42.0);
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
    fn rejected_order_events_are_logged_with_the_pair() {
        let mut app = App::new("dry_run", "momentum");
        app.on_app_event(&AppEvent::OrderRejected { pair: Pair::from("XBT/USD"), reason: "insufficient capital".into() });
        assert!(app.events.back().unwrap().contains("XBT/USD"));
    }
}
