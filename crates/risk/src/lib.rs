//! `risk`: the sole veto point between a strategy's signals and the
//! executor. Owns per-trust-tier position sizing, stop-loss/take-profit
//! (checked every price tick, not just when a strategy fires), and the
//! daily-loss circuit breaker.

pub mod circuit_breaker;
pub mod config;
pub mod position;
pub mod risk_manager;

pub use circuit_breaker::{BreakerState, CircuitBreaker};
pub use config::{default_tiers, RiskConfig, TierConfig};
pub use position::OpenPosition;
pub use risk_manager::{RiskDecision, RiskManager};
