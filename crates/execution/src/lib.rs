//! `execution`: Jupiter (quote/swap) + Jito (MEV-protected bundle landing)
//! for graduated tokens, and a direct pump.fun bonding-curve client for
//! tokens still pre-graduation (Jupiter doesn't route those - confirmed via
//! research). `Executor` routes by `TokenMeta.phase` and treats `dry_run`
//! as first-class: both paths make their real quote call and stop before
//! anything is signed or submitted.

pub mod error;
pub mod executor;
pub mod jito;
pub mod jupiter;
pub mod pumpfun;

pub use error::ExecutionError;
pub use executor::{BondingCurveContext, Executor};
pub use jito::JitoClient;
pub use jupiter::{JupiterClient, QuoteResponse};
pub use pumpfun::PumpFunAccounts;
