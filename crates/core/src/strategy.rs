//! The plug-in interface every trading strategy implements. `bin/trading-bot`
//! and `bin/backtest` both build strategies through [`build_strategy`]-style
//! factories elsewhere in the workspace (see `strategies::build_strategy`),
//! which is what guarantees identical strategy behavior live and in backtests.

use crate::types::{Fill, PriceTick, Signal};

pub trait Strategy: Send {
    /// Human-readable name, used in logs, storage, and the TUI.
    fn name(&self) -> &str;

    /// Called on every new price observation for a mint this strategy is
    /// watching. May return zero or more signals.
    fn on_price_tick(&mut self, tick: &PriceTick) -> Vec<Signal>;

    /// Called when one of this strategy's orders fills. Default no-op; a
    /// stateful strategy (e.g. grid) overrides this to track which levels
    /// are currently held.
    fn on_fill(&mut self, _fill: &Fill) {}

    /// Reset all internal state. Required so the backtester can run the same
    /// strategy instance across multiple parameter sweeps without carrying
    /// state from a previous run.
    fn reset(&mut self);
}
