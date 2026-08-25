//! Plain, synchronous functions over a `rusqlite::Connection`. Kept
//! independent of the actor/channel wiring in `actor.rs` so they're
//! directly unit-testable against an in-memory database.

use bot_core::{Fill, LogLevel, MarketType, OrderReason, Pair, Position, PositionStatus, Side};
use rusqlite::{params, Connection, OptionalExtension};

use crate::error::StorageError;

const SCHEMA: &str = include_str!("schema.sql");

pub fn init_schema(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

fn side_str(side: Side) -> &'static str {
    match side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    }
}

fn parse_side(s: &str) -> Result<Side, StorageError> {
    match s {
        "buy" => Ok(Side::Buy),
        "sell" => Ok(Side::Sell),
        other => Err(StorageError::InvalidValue { field: "side", value: other.to_string() }),
    }
}

fn market_type_str(mt: MarketType) -> &'static str {
    match mt {
        MarketType::Spot => "spot",
        MarketType::Margin => "margin",
        MarketType::Futures => "futures",
    }
}

fn parse_market_type(s: &str) -> Result<MarketType, StorageError> {
    match s {
        "spot" => Ok(MarketType::Spot),
        "margin" => Ok(MarketType::Margin),
        "futures" => Ok(MarketType::Futures),
        other => Err(StorageError::InvalidValue { field: "market_type", value: other.to_string() }),
    }
}

fn reason_str(reason: OrderReason) -> &'static str {
    match reason {
        OrderReason::Strategy => "strategy",
        OrderReason::StopLoss => "stop_loss",
        OrderReason::TakeProfit => "take_profit",
        OrderReason::LiquidationGuard => "liquidation_guard",
    }
}

fn parse_reason(s: &str) -> Result<OrderReason, StorageError> {
    match s {
        "strategy" => Ok(OrderReason::Strategy),
        "stop_loss" => Ok(OrderReason::StopLoss),
        "take_profit" => Ok(OrderReason::TakeProfit),
        "liquidation_guard" => Ok(OrderReason::LiquidationGuard),
        other => Err(StorageError::InvalidValue { field: "reason", value: other.to_string() }),
    }
}

// ---------------------------------------------------------------------
// Trades
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub id: Option<i64>,
    pub ts: i64,
    pub pair: Pair,
    pub market_type: MarketType,
    pub side: Side,
    pub qty: f64,
    pub price: f64,
    pub quote_amount: f64,
    pub fee_quote: f64,
    pub funding_paid_quote: f64,
    pub slippage_bps: Option<u16>,
    pub strategy: String,
    pub reason: OrderReason,
    pub order_id: Option<String>,
    pub status: String,
    pub dry_run: bool,
}

impl TradeRecord {
    pub fn from_fill(fill: &Fill) -> Self {
        let status = if fill.dry_run {
            "dry_run"
        } else if fill.order_id.is_some() {
            "landed"
        } else {
            "pending"
        };
        Self {
            id: None,
            ts: fill.ts,
            pair: fill.pair.clone(),
            market_type: fill.market_type,
            side: fill.side,
            qty: fill.qty,
            price: fill.price,
            quote_amount: fill.quote_amount,
            fee_quote: fill.fee_quote,
            funding_paid_quote: fill.funding_paid_quote,
            slippage_bps: fill.slippage_bps,
            strategy: fill.strategy.clone(),
            reason: fill.reason,
            order_id: fill.order_id.clone(),
            status: status.to_string(),
            dry_run: fill.dry_run,
        }
    }
}

pub fn insert_trade(conn: &Connection, t: &TradeRecord) -> Result<i64, StorageError> {
    conn.execute(
        "INSERT INTO trades (ts, pair, market_type, side, qty, price, quote_amount, fee_quote,
                              funding_paid_quote, slippage_bps, strategy, reason, order_id, status, dry_run)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            t.ts,
            t.pair.as_str(),
            market_type_str(t.market_type),
            side_str(t.side),
            t.qty,
            t.price,
            t.quote_amount,
            t.fee_quote,
            t.funding_paid_quote,
            t.slippage_bps,
            t.strategy,
            reason_str(t.reason),
            t.order_id,
            t.status,
            t.dry_run as i64,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn recent_trades(conn: &Connection, limit: usize) -> Result<Vec<TradeRecord>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT id, ts, pair, market_type, side, qty, price, quote_amount, fee_quote,
                funding_paid_quote, slippage_bps, strategy, reason, order_id, status, dry_run
         FROM trades ORDER BY ts DESC, id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, f64>(5)?,
            row.get::<_, f64>(6)?,
            row.get::<_, f64>(7)?,
            row.get::<_, f64>(8)?,
            row.get::<_, f64>(9)?,
            row.get::<_, Option<i64>>(10)?,
            row.get::<_, String>(11)?,
            row.get::<_, String>(12)?,
            row.get::<_, Option<String>>(13)?,
            row.get::<_, String>(14)?,
            row.get::<_, i64>(15)?,
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (id, ts, pair, market_type, side, qty, price, quote_amount, fee_quote, funding_paid_quote,
            slippage_bps, strategy, reason, order_id, status, dry_run) = row?;
        out.push(TradeRecord {
            id: Some(id),
            ts,
            pair: Pair::from(pair),
            market_type: parse_market_type(&market_type)?,
            side: parse_side(&side)?,
            qty,
            price,
            quote_amount,
            fee_quote,
            funding_paid_quote,
            slippage_bps: slippage_bps.map(|v| v as u16),
            strategy,
            reason: parse_reason(&reason)?,
            order_id,
            status,
            dry_run: dry_run != 0,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------
// Positions
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PositionRow {
    pub id: i64,
    pub position: Position,
}

pub fn insert_open_position(conn: &Connection, p: &Position) -> Result<i64, StorageError> {
    conn.execute(
        "INSERT INTO positions (pair, market_type, opened_ts, closed_ts, entry_price, exit_price, qty,
                                 leverage, liquidation_price, strategy, realized_pnl_quote, status)
         VALUES (?1, ?2, ?3, NULL, ?4, NULL, ?5, ?6, ?7, ?8, NULL, 'open')",
        params![
            p.pair.as_str(),
            market_type_str(p.market_type),
            p.opened_ts,
            p.entry_price,
            p.qty,
            p.leverage,
            p.liquidation_price,
            p.strategy,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn close_position(conn: &Connection, id: i64, exit_price: f64, realized_pnl_quote: f64, closed_ts: i64) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE positions SET exit_price = ?1, realized_pnl_quote = ?2, closed_ts = ?3, status = 'closed'
         WHERE id = ?4",
        params![exit_price, realized_pnl_quote, closed_ts, id],
    )?;
    Ok(())
}

pub fn open_positions(conn: &Connection) -> Result<Vec<PositionRow>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT id, pair, market_type, opened_ts, closed_ts, entry_price, exit_price, qty,
                leverage, liquidation_price, strategy, realized_pnl_quote, status
         FROM positions WHERE status = 'open' ORDER BY opened_ts DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, f64>(5)?,
            row.get::<_, Option<f64>>(6)?,
            row.get::<_, f64>(7)?,
            row.get::<_, f64>(8)?,
            row.get::<_, Option<f64>>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, Option<f64>>(11)?,
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (id, pair, market_type, opened_ts, closed_ts, entry_price, exit_price, qty, leverage,
            liquidation_price, strategy, realized_pnl_quote) = row?;
        out.push(PositionRow {
            id,
            position: Position {
                pair: Pair::from(pair),
                market_type: parse_market_type(&market_type)?,
                opened_ts,
                closed_ts,
                entry_price,
                exit_price,
                qty,
                leverage,
                liquidation_price,
                strategy,
                realized_pnl_quote,
                status: PositionStatus::Open,
            },
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------
// Equity curve
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct EquitySnapshotRecord {
    pub ts: i64,
    pub equity_quote: f64,
    pub realized_pnl_quote: f64,
    pub unrealized_pnl_quote: f64,
    pub daily_pnl_quote: f64,
    pub daily_funding_quote: f64,
}

pub fn insert_equity_snapshot(conn: &Connection, s: &EquitySnapshotRecord) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO equity_curve (ts, equity_quote, realized_pnl_quote, unrealized_pnl_quote, daily_pnl_quote, daily_funding_quote)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![s.ts, s.equity_quote, s.realized_pnl_quote, s.unrealized_pnl_quote, s.daily_pnl_quote, s.daily_funding_quote],
    )?;
    Ok(())
}

pub fn recent_equity_curve(conn: &Connection, limit: usize) -> Result<Vec<EquitySnapshotRecord>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT ts, equity_quote, realized_pnl_quote, unrealized_pnl_quote, daily_pnl_quote, daily_funding_quote
         FROM equity_curve ORDER BY ts DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok(EquitySnapshotRecord {
            ts: row.get(0)?,
            equity_quote: row.get(1)?,
            realized_pnl_quote: row.get(2)?,
            unrealized_pnl_quote: row.get(3)?,
            daily_pnl_quote: row.get(4)?,
            daily_funding_quote: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// ---------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct EventRecord {
    pub ts: i64,
    pub level: LogLevel,
    pub kind: String,
    pub message: String,
}

fn level_str(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    }
}

fn parse_level(s: &str) -> Result<LogLevel, StorageError> {
    match s {
        "info" => Ok(LogLevel::Info),
        "warn" => Ok(LogLevel::Warn),
        "error" => Ok(LogLevel::Error),
        other => Err(StorageError::InvalidValue { field: "level", value: other.to_string() }),
    }
}

pub fn insert_event(conn: &Connection, e: &EventRecord) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO events (ts, level, kind, message) VALUES (?1, ?2, ?3, ?4)",
        params![e.ts, level_str(e.level), e.kind, e.message],
    )?;
    Ok(())
}

pub fn recent_events(conn: &Connection, limit: usize) -> Result<Vec<EventRecord>, StorageError> {
    let mut stmt = conn.prepare("SELECT ts, level, kind, message FROM events ORDER BY ts DESC, id DESC LIMIT ?1")?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (ts, level, kind, message) = row?;
        out.push(EventRecord { ts, level: parse_level(&level)?, kind, message });
    }
    Ok(out)
}

/// Convenience used by tests and by callers that want "does a row exist" without a full query.
pub fn trade_count(conn: &Connection) -> Result<i64, StorageError> {
    Ok(conn.query_row("SELECT COUNT(*) FROM trades", [], |r| r.get(0)).optional()?.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::{OrderReason, Side};

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    fn sample_fill(pair: Pair) -> Fill {
        Fill {
            pair,
            side: Side::Buy,
            qty: 10.0,
            price: 1.5,
            quote_amount: 15.0,
            fee_quote: 0.01,
            funding_paid_quote: 0.0,
            slippage_bps: Some(50),
            market_type: MarketType::Spot,
            strategy: "momentum".into(),
            reason: OrderReason::Strategy,
            order_id: None,
            dry_run: true,
            ts: 1000,
        }
    }

    #[test]
    fn schema_applies_cleanly_and_is_idempotent() {
        let conn = mem_conn();
        // Applying again must not error (CREATE TABLE IF NOT EXISTS).
        init_schema(&conn).unwrap();
    }

    #[test]
    fn insert_and_read_back_trade_round_trips() {
        let conn = mem_conn();
        let pair = Pair::from("XBT/USD");
        let fill = sample_fill(pair.clone());
        let id = insert_trade(&conn, &TradeRecord::from_fill(&fill)).unwrap();
        assert!(id > 0);
        assert_eq!(trade_count(&conn).unwrap(), 1);

        let trades = recent_trades(&conn, 10).unwrap();
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].pair, pair);
        assert_eq!(trades[0].side, Side::Buy);
        assert_eq!(trades[0].status, "dry_run");
        assert!((trades[0].price - 1.5).abs() < 1e-9);
    }

    #[test]
    fn position_lifecycle_open_then_close() {
        let conn = mem_conn();
        let pair = Pair::from("XBT/USD");
        let pos = Position {
            pair: pair.clone(),
            market_type: MarketType::Margin,
            opened_ts: 100,
            closed_ts: None,
            entry_price: 2.0,
            exit_price: None,
            qty: 5.0,
            leverage: 2.0,
            liquidation_price: Some(1.0),
            strategy: "grid".into(),
            realized_pnl_quote: None,
            status: PositionStatus::Open,
        };
        let id = insert_open_position(&conn, &pos).unwrap();

        let open = open_positions(&conn).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, id);
        assert_eq!(open[0].position.pair, pair);
        assert_eq!(open[0].position.market_type, MarketType::Margin);
        assert_eq!(open[0].position.liquidation_price, Some(1.0));

        close_position(&conn, id, 2.5, 2.5, 200).unwrap();
        let open_after = open_positions(&conn).unwrap();
        assert!(open_after.is_empty(), "closed position should no longer be listed as open");
    }

    #[test]
    fn equity_curve_round_trips_in_order() {
        let conn = mem_conn();
        for (i, eq) in [10.0, 10.5, 9.8].into_iter().enumerate() {
            insert_equity_snapshot(&conn, &EquitySnapshotRecord {
                ts: i as i64,
                equity_quote: eq,
                realized_pnl_quote: 0.0,
                unrealized_pnl_quote: 0.0,
                daily_pnl_quote: 0.0,
                daily_funding_quote: 0.0,
            }).unwrap();
        }
        let curve = recent_equity_curve(&conn, 10).unwrap();
        assert_eq!(curve.len(), 3);
        assert_eq!(curve[0].ts, 2); // DESC order - most recent first
    }

    #[test]
    fn events_round_trip_with_level() {
        let conn = mem_conn();
        insert_event(&conn, &EventRecord {
            ts: 5,
            level: LogLevel::Error,
            kind: "circuit_breaker".into(),
            message: "daily loss limit breached".into(),
        }).unwrap();
        let events = recent_events(&conn, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].level, LogLevel::Error);
        assert_eq!(events[0].kind, "circuit_breaker");
    }

    #[test]
    fn recent_trades_respects_limit() {
        let conn = mem_conn();
        let pair = Pair::from("XBT/USD");
        for i in 0..5 {
            let mut fill = sample_fill(pair.clone());
            fill.ts = i;
            insert_trade(&conn, &TradeRecord::from_fill(&fill)).unwrap();
        }
        let trades = recent_trades(&conn, 2).unwrap();
        assert_eq!(trades.len(), 2);
    }

    #[test]
    fn liquidation_guard_reason_round_trips() {
        let conn = mem_conn();
        let pair = Pair::from("XBT/USD");
        let mut fill = sample_fill(pair);
        fill.reason = OrderReason::LiquidationGuard;
        fill.market_type = MarketType::Futures;
        insert_trade(&conn, &TradeRecord::from_fill(&fill)).unwrap();
        let trades = recent_trades(&conn, 1).unwrap();
        assert_eq!(trades[0].reason, OrderReason::LiquidationGuard);
        assert_eq!(trades[0].market_type, MarketType::Futures);
    }
}
