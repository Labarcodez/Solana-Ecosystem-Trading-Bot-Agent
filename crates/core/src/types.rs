//! Domain types shared by every crate in the workspace. This module has zero
//! I/O dependencies on purpose: strategies, the risk manager, and the
//! backtester all build on these types without pulling in async runtime or
//! network machinery.
//!
//! These types were retyped for Kraken in this pivot: every identifier that
//! used to be a Solana `solana_sdk::Pubkey` (a mint address) is now a
//! [`Pair`] (a Kraken trading pair symbol, e.g. `"XBT/USD"`), and the old
//! `TokenPhase`/`TrustTier` (bonding_curve/migrated) split is now
//! [`MarketType`]/[`RiskTier`] (spot/margin/futures) - the axis that
//! actually matters for risk on a centralized exchange.

use serde::{Deserialize, Serialize};

/// A Kraken tradable pair symbol (e.g. `"XBT/USD"`, `"ETH/XBT"`). Kept as a
/// thin newtype rather than a bare `String` so a pair identifier can't be
/// silently confused with an arbitrary string elsewhere in the pipeline -
/// the same role `solana_sdk::Pubkey` played for a mint address before this
/// pivot.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Pair(pub String);

impl Pair {
    pub fn new(symbol: impl Into<String>) -> Self {
        Self(symbol.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Pair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for Pair {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for Pair {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Buy or sell, shared by signals, approved orders, and fills so the same
/// value flows unchanged from strategy -> risk -> executor -> storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

/// Which Kraken product an order/position lives in. This is what the
/// `execution` crate uses to route to the spot, margin, or futures client,
/// and what `risk` uses to pick a tier - the direct replacement for the old
/// Solana build's `TokenPhase` (bonding_curve/migrated).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MarketType {
    /// Plain spot, no leverage.
    #[default]
    Spot,
    /// Spot trading on margin (Kraken's `leverage` parameter on `AddOrder`).
    Margin,
    /// Kraken Futures perpetual contract.
    Futures,
}

/// How much risk budget a position/order gets. Assigned directly from
/// `MarketType` - leveraged and derivative positions are inherently riskier
/// than plain spot, so they get their own tighter tier in `risk`. Direct
/// replacement for the old build's `TrustTier` (bonding_curve/migrated_new/
/// established), which was about *how well-known a token was*; this is
/// about *how much leverage/liquidation risk a position carries*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RiskTier {
    Spot,
    Margin,
    Futures,
}

impl From<MarketType> for RiskTier {
    fn from(market_type: MarketType) -> Self {
        match market_type {
            MarketType::Spot => RiskTier::Spot,
            MarketType::Margin => RiskTier::Margin,
            MarketType::Futures => RiskTier::Futures,
        }
    }
}

/// Everything the pipeline knows about a tradable pair at a point in time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairMeta {
    pub pair: Pair,
    pub market_type: MarketType,
    pub risk_tier: RiskTier,
    /// Leverage in effect for this pair/order. Always `1.0` for `Spot`.
    pub leverage: f64,
}

/// A single price observation for a pair, from Kraken's ticker/OHLC feed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceTick {
    pub pair: Pair,
    pub price: f64,
    /// Kraken Futures perpetuals only: the contract's current hourly
    /// funding rate (a fraction, not a percent - e.g. `0.0001` = 0.01%/hr).
    /// `None` for every spot/margin tick and for any futures tick where the
    /// rate wasn't available. This is what `strategies::FundingCarryStrategy`
    /// reacts to.
    #[serde(default)]
    pub funding_rate: Option<f64>,
    pub ts: i64,
}

/// A trading intent produced by a `Strategy`. Not yet sized or approved -
/// that is `risk`'s job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub side: Side,
    pub pair: Pair,
    /// Confidence in [0.0, 1.0], used by the risk manager to scale position size.
    pub strength: f64,
    pub reason: String,
    pub strategy: String,
    pub ts: i64,
}

/// Why an `ApprovedOrder` exists: a strategy signal, or a risk-manager
/// initiated protective exit. Protective exits are synthesized directly by
/// `risk` and bypass the strategy entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderReason {
    Strategy,
    StopLoss,
    TakeProfit,
    /// Margin/futures only: closed because the liquidation-distance guard
    /// (see `risk`) tripped, not because price crossed a configured SL/TP.
    LiquidationGuard,
}

/// An order that has cleared risk management and is ready for `execution`.
/// This is the only shape `execution` ever receives an order in - there is
/// no path from `Signal` to a trade that skips `risk`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedOrder {
    pub side: Side,
    pub pair: Pair,
    /// Position size denominated in the pair's quote currency (e.g. USD for
    /// `XBT/USD`) - the direct replacement for the old build's SOL-
    /// denominated `size_sol`.
    pub size_quote: f64,
    pub max_slippage_bps: u16,
    pub reason: OrderReason,
    pub pair_meta: PairMeta,
    pub strategy: String,
    pub ts: i64,
}

/// The on-chain-equivalent (or dry-run simulated) result of executing an
/// `ApprovedOrder` on Kraken.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub pair: Pair,
    pub side: Side,
    pub qty: f64,
    pub price: f64,
    pub quote_amount: f64,
    pub fee_quote: f64,
    /// Futures only: funding paid (positive) or received (negative) against
    /// this position since it was opened/last marked. `0.0` for spot/margin.
    pub funding_paid_quote: f64,
    pub slippage_bps: Option<u16>,
    pub market_type: MarketType,
    pub strategy: String,
    pub reason: OrderReason,
    /// Kraken's own order/transaction id (`txid`), when live.
    pub order_id: Option<String>,
    pub dry_run: bool,
    pub ts: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PositionStatus {
    Open,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub pair: Pair,
    pub market_type: MarketType,
    pub opened_ts: i64,
    pub closed_ts: Option<i64>,
    pub entry_price: f64,
    pub exit_price: Option<f64>,
    pub qty: f64,
    pub leverage: f64,
    /// Only meaningful for `Margin`/`Futures` positions - the price at
    /// which this position would be force-closed. `risk`'s
    /// liquidation-distance guard is built on this field.
    pub liquidation_price: Option<f64>,
    pub strategy: String,
    pub realized_pnl_quote: Option<f64>,
    pub status: PositionStatus,
}

/// A point-in-time snapshot of the bot's overall state, published on a
/// `watch` channel for the TUI's "current state" widgets so a new subscriber
/// never needs history replay.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub equity_quote: f64,
    pub realized_pnl_quote: f64,
    pub unrealized_pnl_quote: f64,
    pub daily_pnl_quote: f64,
    /// Futures only: cumulative funding paid (positive) / received
    /// (negative) today, tracked separately from price PnL so the circuit
    /// breaker can distinguish "losing to the market" from "bleeding to
    /// funding" (see `risk`'s honesty notes).
    pub daily_funding_quote: f64,
    pub open_positions: usize,
    pub circuit_breaker_tripped: bool,
}

/// Severity used for both the `events` storage table and the TUI log widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// Everything that flows on the broadcast event bus: fills, rejections, and
/// circuit-breaker state changes. Consumed by `storage`, `tui`, and `risk`
/// (for PnL/position feedback).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AppEvent {
    Fill(Fill),
    OrderRejected { pair: Pair, reason: String },
    CircuitBreakerTripped { reason: String, ts: i64 },
    CircuitBreakerReset { ts: i64 },
    Log { level: LogLevel, message: String, ts: i64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_displays_as_its_symbol() {
        let pair = Pair::from("XBT/USD");
        assert_eq!(pair.as_str(), "XBT/USD");
        assert_eq!(pair.to_string(), "XBT/USD");
    }

    #[test]
    fn pairs_with_the_same_symbol_are_equal_and_hash_equal() {
        use std::collections::HashSet;
        let a = Pair::from("ETH/USD");
        let b = Pair::from("ETH/USD".to_string());
        assert_eq!(a, b);
        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
    }

    #[test]
    fn market_type_maps_onto_the_matching_risk_tier() {
        assert_eq!(RiskTier::from(MarketType::Spot), RiskTier::Spot);
        assert_eq!(RiskTier::from(MarketType::Margin), RiskTier::Margin);
        assert_eq!(RiskTier::from(MarketType::Futures), RiskTier::Futures);
    }
}
