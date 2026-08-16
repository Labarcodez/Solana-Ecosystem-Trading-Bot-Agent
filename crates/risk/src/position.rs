use bot_core::TokenMeta;

/// Risk-manager-internal view of an open position. Distinct from
/// `bot_core::Position` (the storage-facing record): this one carries the
/// effective SL/TP percentages, the `TokenMeta` needed to route an exit
/// order through the right executor, and a `pending_exit` flag used to
/// avoid re-triggering a protective sell every tick while an exit order is
/// already in flight.
#[derive(Debug, Clone)]
pub struct OpenPosition {
    pub token_meta: TokenMeta,
    pub entry_price: f64,
    pub qty: f64,
    pub size_sol: f64,
    pub strategy: String,
    pub opened_ts: i64,
    pub stop_loss_pct: f64,
    pub take_profit_pct: Option<f64>,
    pub pending_exit: bool,
}

impl OpenPosition {
    /// Unrealized PnL in SOL at the given current price.
    pub fn unrealized_pnl_sol(&self, current_price: f64) -> f64 {
        (current_price - self.entry_price) * self.qty
    }

    pub fn pnl_pct(&self, current_price: f64) -> f64 {
        if self.entry_price == 0.0 {
            return 0.0;
        }
        (current_price - self.entry_price) / self.entry_price * 100.0
    }
}
