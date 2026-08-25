//! Storage runs on its own dedicated OS thread owning the single
//! `rusqlite::Connection` (SQLite prefers one writer anyway, and rusqlite is
//! synchronous). Callers talk to it through [`StorageHandle`], which sends a
//! command over a plain `std::sync::mpsc` channel (a non-blocking, fast
//! send even from async code) and awaits a `tokio::sync::oneshot` reply.
//! This avoids sprinkling `Arc<Mutex<Connection>>` or `spawn_blocking`
//! through the rest of the codebase.

use std::sync::mpsc as std_mpsc;
use std::thread;

use bot_core::{Fill, LogLevel, Position};
use rusqlite::Connection;
use tokio::sync::oneshot;

use crate::error::StorageError;
use crate::repo::{self, EquitySnapshotRecord, EventRecord, PositionRow, TradeRecord};

type Reply<T> = oneshot::Sender<Result<T, StorageError>>;

enum StorageCommand {
    InsertTrade { trade: TradeRecord, reply: Reply<i64> },
    InsertOpenPosition { position: Position, reply: Reply<i64> },
    ClosePosition { id: i64, exit_price: f64, realized_pnl_quote: f64, closed_ts: i64, reply: Reply<()> },
    OpenPositions { reply: Reply<Vec<PositionRow>> },
    InsertEquitySnapshot { snapshot: EquitySnapshotRecord, reply: Reply<()> },
    RecentEquityCurve { limit: usize, reply: Reply<Vec<EquitySnapshotRecord>> },
    InsertEvent { event: EventRecord, reply: Reply<()> },
    RecentEvents { limit: usize, reply: Reply<Vec<EventRecord>> },
    RecentTrades { limit: usize, reply: Reply<Vec<TradeRecord>> },
    Shutdown,
}

#[derive(Clone)]
pub struct StorageHandle {
    tx: std_mpsc::Sender<StorageCommand>,
}

impl StorageHandle {
    /// Open (or create) the SQLite database at `path`, apply the schema, and
    /// spawn the dedicated storage thread. Pass `":memory:"` for an
    /// in-memory database (used in tests and `--dry-run`/mock demos).
    pub fn spawn(path: &str) -> Result<Self, StorageError> {
        let conn = Connection::open(path)?;
        repo::init_schema(&conn)?;

        let (tx, rx) = std_mpsc::channel::<StorageCommand>();
        thread::Builder::new()
            .name("storage".into())
            .spawn(move || run(conn, rx))
            .expect("failed to spawn storage thread");

        Ok(Self { tx })
    }

    async fn call<T>(&self, build: impl FnOnce(Reply<T>) -> StorageCommand) -> Result<T, StorageError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(build(reply_tx))
            .map_err(|_| StorageError::ActorGone)?;
        reply_rx.await.map_err(|_| StorageError::ActorGone)?
    }

    pub async fn insert_trade(&self, fill: &Fill) -> Result<i64, StorageError> {
        let trade = TradeRecord::from_fill(fill);
        self.call(|reply| StorageCommand::InsertTrade { trade, reply }).await
    }

    pub async fn insert_open_position(&self, position: Position) -> Result<i64, StorageError> {
        self.call(|reply| StorageCommand::InsertOpenPosition { position, reply }).await
    }

    pub async fn close_position(
        &self,
        id: i64,
        exit_price: f64,
        realized_pnl_quote: f64,
        closed_ts: i64,
    ) -> Result<(), StorageError> {
        self.call(|reply| StorageCommand::ClosePosition { id, exit_price, realized_pnl_quote, closed_ts, reply })
            .await
    }

    pub async fn open_positions(&self) -> Result<Vec<PositionRow>, StorageError> {
        self.call(|reply| StorageCommand::OpenPositions { reply }).await
    }

    pub async fn insert_equity_snapshot(&self, snapshot: EquitySnapshotRecord) -> Result<(), StorageError> {
        self.call(|reply| StorageCommand::InsertEquitySnapshot { snapshot, reply }).await
    }

    pub async fn recent_equity_curve(&self, limit: usize) -> Result<Vec<EquitySnapshotRecord>, StorageError> {
        self.call(|reply| StorageCommand::RecentEquityCurve { limit, reply }).await
    }

    pub async fn insert_event(&self, ts: i64, level: LogLevel, kind: &str, message: &str) -> Result<(), StorageError> {
        let event = EventRecord { ts, level, kind: kind.to_string(), message: message.to_string() };
        self.call(|reply| StorageCommand::InsertEvent { event, reply }).await
    }

    pub async fn recent_events(&self, limit: usize) -> Result<Vec<EventRecord>, StorageError> {
        self.call(|reply| StorageCommand::RecentEvents { limit, reply }).await
    }

    pub async fn recent_trades(&self, limit: usize) -> Result<Vec<TradeRecord>, StorageError> {
        self.call(|reply| StorageCommand::RecentTrades { limit, reply }).await
    }

    /// Signal the storage thread to exit. Best-effort - dropping the handle
    /// (and all its clones) also ends the thread once the channel closes.
    pub fn shutdown(&self) {
        let _ = self.tx.send(StorageCommand::Shutdown);
    }
}

fn run(conn: Connection, rx: std_mpsc::Receiver<StorageCommand>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            StorageCommand::InsertTrade { trade, reply } => {
                let _ = reply.send(repo::insert_trade(&conn, &trade));
            }
            StorageCommand::InsertOpenPosition { position, reply } => {
                let _ = reply.send(repo::insert_open_position(&conn, &position));
            }
            StorageCommand::ClosePosition { id, exit_price, realized_pnl_quote, closed_ts, reply } => {
                let _ = reply.send(repo::close_position(&conn, id, exit_price, realized_pnl_quote, closed_ts));
            }
            StorageCommand::OpenPositions { reply } => {
                let _ = reply.send(repo::open_positions(&conn));
            }
            StorageCommand::InsertEquitySnapshot { snapshot, reply } => {
                let _ = reply.send(repo::insert_equity_snapshot(&conn, &snapshot));
            }
            StorageCommand::RecentEquityCurve { limit, reply } => {
                let _ = reply.send(repo::recent_equity_curve(&conn, limit));
            }
            StorageCommand::InsertEvent { event, reply } => {
                let _ = reply.send(repo::insert_event(&conn, &event));
            }
            StorageCommand::RecentEvents { limit, reply } => {
                let _ = reply.send(repo::recent_events(&conn, limit));
            }
            StorageCommand::RecentTrades { limit, reply } => {
                let _ = reply.send(repo::recent_trades(&conn, limit));
            }
            StorageCommand::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::{OrderReason, Side};

    #[tokio::test]
    async fn actor_round_trips_a_trade_over_the_channel() {
        let handle = StorageHandle::spawn(":memory:").unwrap();
        let pair = bot_core::Pair::from("XBT/USD");
        let fill = Fill {
            pair, side: Side::Buy, qty: 1.0, price: 1.0, quote_amount: 1.0,
            fee_quote: 0.0, funding_paid_quote: 0.0, slippage_bps: None,
            market_type: bot_core::MarketType::Spot,
            strategy: "test".into(), reason: OrderReason::Strategy,
            order_id: None, dry_run: true, ts: 0,
        };
        let id = handle.insert_trade(&fill).await.unwrap();
        assert!(id > 0);
        let trades = handle.recent_trades(10).await.unwrap();
        assert_eq!(trades.len(), 1);
        handle.shutdown();
    }

    #[tokio::test]
    async fn actor_survives_concurrent_callers() {
        let handle = StorageHandle::spawn(":memory:").unwrap();
        let mut tasks = Vec::new();
        for i in 0..20 {
            let h = handle.clone();
            tasks.push(tokio::spawn(async move {
                h.insert_event(i, LogLevel::Info, "test", "concurrent write").await.unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        let events = handle.recent_events(100).await.unwrap();
        assert_eq!(events.len(), 20);
    }
}
