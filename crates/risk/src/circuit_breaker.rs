//! Daily-loss circuit breaker. Explicit design rule (documented in the
//! project README, not just here): a tripped breaker blocks new Buy
//! approvals only. It never blocks a Sell - neither a strategy-initiated
//! exit nor a stop-loss/take-profit exit - because trapping the bot in
//! losing positions it can no longer sell out of would turn a loss limiter
//! into the opposite of protection.

#[derive(Debug, Clone, PartialEq)]
pub enum BreakerState {
    Normal,
    Tripped { since_ts: i64, reason: String },
}

pub struct CircuitBreaker {
    state: BreakerState,
    daily_loss_limit_quote: f64,
    daily_loss_limit_pct: f64,
}

impl CircuitBreaker {
    pub fn new(daily_loss_limit_quote: f64, daily_loss_limit_pct: f64) -> Self {
        Self {
            state: BreakerState::Normal,
            daily_loss_limit_quote,
            daily_loss_limit_pct,
        }
    }

    pub fn is_tripped(&self) -> bool {
        matches!(self.state, BreakerState::Tripped { .. })
    }

    pub fn state(&self) -> &BreakerState {
        &self.state
    }

    /// Evaluate today's PnL (realized + unrealized) against the configured
    /// limits and trip if breached. `starting_capital_quote` is the capital
    /// the daily % limit is measured against. Returns `Some(reason)` if this
    /// call is the one that tripped the breaker (so the caller can emit an
    /// `AppEvent::CircuitBreakerTripped` exactly once).
    pub fn check(&mut self, daily_pnl_quote: f64, starting_capital_quote: f64, ts: i64) -> Option<String> {
        if self.is_tripped() || daily_pnl_quote >= 0.0 {
            return None;
        }
        let loss_quote = -daily_pnl_quote;
        let loss_pct = if starting_capital_quote > 0.0 {
            loss_quote / starting_capital_quote * 100.0
        } else {
            0.0
        };

        let breached_abs = loss_quote >= self.daily_loss_limit_quote;
        let breached_pct = loss_pct >= self.daily_loss_limit_pct;
        if breached_abs || breached_pct {
            let reason = format!(
                "daily loss {loss_quote:.4} ({loss_pct:.2}%) breached limit ({} / {}%)",
                self.daily_loss_limit_quote, self.daily_loss_limit_pct
            );
            self.state = BreakerState::Tripped { since_ts: ts, reason: reason.clone() };
            return Some(reason);
        }
        None
    }

    /// Reset at UTC midnight or on manual restart.
    pub fn reset(&mut self) {
        self.state = BreakerState::Normal;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_trip_under_limit() {
        let mut b = CircuitBreaker::new(1.0, 50.0);
        assert!(b.check(-0.5, 10.0, 0).is_none());
        assert!(!b.is_tripped());
    }

    #[test]
    fn trips_on_absolute_quote_limit() {
        let mut b = CircuitBreaker::new(1.0, 100.0);
        assert!(b.check(-1.5, 100.0, 0).is_some());
        assert!(b.is_tripped());
    }

    #[test]
    fn trips_on_pct_limit() {
        let mut b = CircuitBreaker::new(1000.0, 10.0);
        assert!(b.check(-1.5, 10.0, 0).is_some()); // 15% loss of 10 units of capital
        assert!(b.is_tripped());
    }

    #[test]
    fn does_not_re_trip_or_re_report_once_tripped() {
        let mut b = CircuitBreaker::new(1.0, 100.0);
        assert!(b.check(-2.0, 100.0, 0).is_some());
        assert!(b.check(-3.0, 100.0, 1).is_none()); // already tripped, no duplicate report
    }

    #[test]
    fn reset_clears_state() {
        let mut b = CircuitBreaker::new(1.0, 100.0);
        b.check(-2.0, 100.0, 0);
        assert!(b.is_tripped());
        b.reset();
        assert!(!b.is_tripped());
    }
}
