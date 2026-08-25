//! `risk`: the sole veto point between a strategy's signals and the
//! executor. Owns per-risk-tier (spot/margin/futures) position sizing,
//! stop-loss/take-profit and the liquidation-distance guard (checked every
//! price tick, not just when a strategy fires), and the daily-loss circuit
//! breaker (which also accounts for futures funding bleed).

pub mod circuit_breaker;
pub mod config;
pub mod position;
pub mod risk_manager;

pub use circuit_breaker::{BreakerState, CircuitBreaker};
pub use config::{default_tiers, RiskConfig, TierConfig};
pub use position::OpenPosition;
pub use risk_manager::{approx_liquidation_distance_pct, approx_liquidation_price, RiskDecision, RiskManager};
