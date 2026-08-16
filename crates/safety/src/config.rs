use serde::{Deserialize, Serialize};

/// Mirrors `config.toml`'s `[safety]` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyConfig {
    pub require_mint_authority_revoked: bool,
    pub require_freeze_authority_revoked: bool,
    pub require_lp_burned_or_locked: bool,
    pub min_liquidity_sol: f64,
    pub max_holder_concentration_pct: f64,
    pub min_pool_age_secs: i64,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            require_mint_authority_revoked: true,
            require_freeze_authority_revoked: true,
            require_lp_burned_or_locked: true,
            min_liquidity_sol: 5.0,
            max_holder_concentration_pct: 40.0,
            min_pool_age_secs: 60,
        }
    }
}

/// Everything a `TokenSafetyScorer` needs about a token's current on-chain
/// state. Deliberately decoupled from *how* it was fetched (RPC calls in
/// `fetcher.rs` for live use, hand-built literals in tests) so scoring
/// logic stays pure and unit-testable without a live RPC connection.
#[derive(Debug, Clone, Copy)]
pub struct OnchainSnapshot {
    pub mint_authority_present: bool,
    pub freeze_authority_present: bool,
    /// Not applicable (and not checked) for a still-bonding-curve token -
    /// there's no separate LP token to burn/lock until it graduates.
    pub lp_burned_or_locked: bool,
    /// % of circulating supply held by the top N holders (LP/burn
    /// addresses excluded by whatever fetched this snapshot).
    pub top_holder_concentration_pct: f64,
    pub liquidity_sol: f64,
    pub age_secs: i64,
}
