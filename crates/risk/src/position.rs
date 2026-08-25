use bot_core::PairMeta;

/// Risk-manager-internal view of an open position. Distinct from
/// `bot_core::Position` (the storage-facing record): this one carries the
/// effective SL/TP percentages, the `PairMeta` needed to route an exit
/// order through the right executor, and a `pending_exit` flag used to
/// avoid re-triggering a protective sell every tick while an exit order is
/// already in flight.
#[derive(Debug, Clone)]
pub struct OpenPosition {
    pub pair_meta: PairMeta,
    pub entry_price: f64,
    pub qty: f64,
    pub size_quote: f64,
    pub strategy: String,
    pub opened_ts: i64,
    pub stop_loss_pct: f64,
    pub take_profit_pct: Option<f64>,
    /// Only meaningful for `Margin`/`Futures` positions - see
    /// `risk_manager::approx_liquidation_distance_pct` for how this is
    /// derived and the honest caveat on what it approximates.
    pub liquidation_price: Option<f64>,
    pub pending_exit: bool,
}

impl OpenPosition {
    /// Unrealized PnL in quote currency at the given current price.
    pub fn unrealized_pnl_quote(&self, current_price: f64) -> f64 {
        (current_price - self.entry_price) * self.qty
    }

    pub fn pnl_pct(&self, current_price: f64) -> f64 {
        if self.entry_price == 0.0 {
            return 0.0;
        }
        (current_price - self.entry_price) / self.entry_price * 100.0
    }

    /// True once price has crossed (or reached) the stored liquidation
    /// price - the mandatory backstop for a leveraged position,
    /// independent of whatever stop-loss percentage is configured.
    ///
    /// Only handles the long case - `risk` is long-only in this build (a
    /// `Signal` only ever opens via `Buy` and closes via `Sell`, the same
    /// model the old Solana build used), so a short futures position isn't
    /// representable yet. Documented as a scope limit, not a silent gap:
    /// see README.
    pub fn liquidation_breached(&self, current_price: f64) -> bool {
        match self.liquidation_price {
            Some(liq) => current_price <= liq,
            None => false,
        }
    }
}
