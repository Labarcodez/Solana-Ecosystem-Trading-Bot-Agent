//! `storage`: SQLite-backed trade/position/equity/event log. All access
//! goes through [`StorageHandle`], which owns the one `rusqlite::Connection`
//! on a dedicated thread - see `actor.rs` for why.

pub mod actor;
pub mod error;
pub mod repo;

pub use actor::StorageHandle;
pub use error::StorageError;
pub use repo::{EquitySnapshotRecord, EventRecord, PositionRow, TradeRecord};
