//! `execution`: hand-rolled Kraken Spot (+ margin) and Kraken Futures REST
//! clients, tied together behind one `Executor` that routes by
//! `ApprovedOrder`'s `MarketType`. `dry_run` is treated as first-class
//! everywhere: every path makes its real public-endpoint call (proving the
//! integration is genuinely live) and stops before anything private is
//! signed or submitted. See `kraken_spot`/`kraken_futures` module docs for
//! exactly what's cross-checked against Kraken's own documentation versus
//! independently verified in this session.

pub mod error;
pub mod executor;
pub mod kraken_futures;
pub mod kraken_spot;

pub use error::ExecutionError;
pub use executor::Executor;
pub use kraken_futures::{FuturesOrderRequest, FuturesSendOrderResult, FuturesTicker, KrakenFuturesClient};
pub use kraken_spot::{AddOrderRequest, AddOrderResult, KrakenSpotClient, OrderType, TickerInfo};
