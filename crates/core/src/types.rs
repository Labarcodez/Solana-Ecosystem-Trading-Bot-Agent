//! Domain types shared by every crate in the workspace. This module has zero
//! I/O dependencies on purpose: strategies, the risk manager, and the
//! backtester all build on these types without pulling in async runtime or
//! network machinery.

use serde::{Deserialize, Serialize};
pub use solana_sdk::pubkey::Pubkey;

/// Buy or sell, shared by signals, approved orders, and fills so the same
/// value flows unchanged from strategy -> risk -> executor -> storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

/// Where a token currently sits in its lifecycle. This is what the
/// `execution` crate uses to decide whether to route through the pump.fun
/// bonding-curve client or through Jupiter+Jito.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenPhase {
    /// Still trading on the pump.fun bonding curve; not yet routed by Jupiter.
    BondingCurve,
    /// Graduated to a real AMM pool (PumpSwap/Raydium/Orca); reachable via Jupiter.
    Migrated,
}

/// How much the risk manager is willing to trust a token. Assigned by the
/// `safety` crate's `TokenSafetyScorer` and re-checked by `risk` at order
/// time. This is the rule-based v1 mechanism described in the README; a
/// learned scorer can later populate the same tier without changing how
/// `risk` consumes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustTier {
    /// Pre-graduation pump.fun token: least known, tightest risk budget.
    BondingCurve,
    /// Recently graduated / thin trading history: small risk budget.
    MigratedNew,
    /// Cleared all safety checks comfortably and/or has local trade history.
    Established,
}

/// Everything the pipeline knows about a token at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenMeta {
    pub mint: Pubkey,
    pub phase: TokenPhase,
    pub trust_tier: TrustTier,
    /// Unix seconds when this token was first observed (by discovery, or by
    /// config for a manually-added token).
    pub discovered_at: i64,
    /// Free-text description of how this token entered the tradable set,
    /// e.g. "pumpfun_create", "pumpswap_pool_init", "config".
    pub source: String,
}

/// A single price observation for a mint, in SOL (or `base_currency` from
/// config) per token.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PriceTick {
    pub mint: Pubkey,
    pub price: f64,
    pub ts: i64,
}

/// A trading intent produced by a `Strategy`. Not yet sized or approved -
/// that is `risk`'s job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub side: Side,
    pub mint: Pubkey,
    /// Confidence in [0.0, 1.0], used by the risk manager to scale position size.
    pub strength: f64,
    pub reason: String,
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
}

/// An order that has cleared risk management and is ready for `execution`.
/// This is the only shape `execution` ever receives an order in - there is
/// no path from `Signal` to a trade that skips `risk`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedOrder {
    pub side: Side,
    pub mint: Pubkey,
    pub size_sol: f64,
    pub max_slippage_bps: u16,
    pub reason: OrderReason,
    pub token_meta: TokenMeta,
    pub strategy: String,
    pub ts: i64,
}

/// The on-chain (or dry-run simulated) result of executing an `ApprovedOrder`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub mint: Pubkey,
    pub side: Side,
    pub qty: f64,
    pub price: f64,
    pub sol_amount: f64,
    pub fee_sol: f64,
    pub jito_tip_sol: f64,
    pub slippage_bps: Option<u16>,
    pub strategy: String,
    pub reason: OrderReason,
    pub tx_signature: Option<String>,
    pub bundle_id: Option<String>,
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
    pub mint: Pubkey,
    pub opened_ts: i64,
    pub closed_ts: Option<i64>,
    pub entry_price: f64,
    pub exit_price: Option<f64>,
    pub qty: f64,
    pub strategy: String,
    pub realized_pnl_sol: Option<f64>,
    pub status: PositionStatus,
}

/// A point-in-time snapshot of the bot's overall state, published on a
/// `watch` channel for the TUI's "current state" widgets so a new subscriber
/// never needs history replay.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub equity_sol: f64,
    pub realized_pnl_sol: f64,
    pub unrealized_pnl_sol: f64,
    pub daily_pnl_sol: f64,
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

/// Everything that flows on the broadcast event bus: fills, rejections,
/// discovery/safety outcomes, and circuit-breaker state changes. Consumed by
/// `storage`, `tui`, and `risk` (for PnL/position feedback).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AppEvent {
    Fill(Fill),
    OrderRejected { mint: Pubkey, reason: String },
    CircuitBreakerTripped { reason: String, ts: i64 },
    CircuitBreakerReset { ts: i64 },
    TokenDiscovered(TokenMeta),
    TokenRejectedBySafety { mint: Pubkey, reasons: Vec<String> },
    Log { level: LogLevel, message: String, ts: i64 },
}
