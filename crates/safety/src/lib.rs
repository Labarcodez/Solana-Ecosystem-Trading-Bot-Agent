//! `safety`: the mandatory pre-trade gate for any token the bot hasn't
//! already established trust in. `RuleBasedScorer` (v1) checks mint/freeze
//! authority, LP burn status, holder concentration, liquidity, and age -
//! see `scorer.rs` for the trait boundary that a future ML-based scorer
//! would implement instead.

pub mod config;
pub mod error;
pub mod fetcher;
pub mod mint_parser;
pub mod scorer;

pub use config::{OnchainSnapshot, SafetyConfig};
pub use error::SafetyError;
pub use fetcher::fetch_onchain_snapshot;
pub use mint_parser::{decode_mint_account, MintInfo};
pub use scorer::{RuleBasedScorer, SafetyVerdict, TokenSafetyScorer};
