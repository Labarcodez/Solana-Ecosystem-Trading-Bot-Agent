//! Fetches the live on-chain data a `TokenSafetyScorer` needs, via a real
//! Alchemy (or any Solana JSON-RPC) endpoint. This module can't be unit
//! tested against live chain state in this sandbox - it's exercised when
//! the user runs the bot with their own `ALCHEMY_RPC_URL` - but the pure
//! decode logic it calls into (`mint_parser`) is fully unit tested.

use std::str::FromStr;

use bot_core::{Pubkey, TokenMeta, TokenPhase};
use solana_client::nonblocking::rpc_client::RpcClient;

use crate::config::OnchainSnapshot;
use crate::error::SafetyError;
use crate::mint_parser::decode_mint_account;

/// Solana's conventional burn/incinerator address - LP tokens sent here are
/// unrecoverable. This is the only "burned" signal v1 recognizes.
///
/// Documented limitation: third-party LP locker programs (Streamflow,
/// Bonfida vesting, and others) are NOT detected - a locked-but-not-burned
/// LP conservatively fails the `lp_burned_or_locked` check rather than
/// silently passing. Extending this to recognize specific locker programs
/// is future work, not something this v1 claims to do.
pub fn burn_address() -> Pubkey {
    Pubkey::from_str("1nc1nerator11111111111111111111111111111111").expect("valid static pubkey")
}

/// Fetches everything the safety scorer needs for `token`.
///
/// `lp_mint` is the LP token mint to check for a burned balance - only
/// meaningful once a token has graduated (`TokenPhase::Migrated`); pass
/// `None` for a still-bonding-curve token. `liquidity_sol` is passed in
/// rather than re-derived here because by the time safety scoring runs,
/// `market_data`/`discovery` already know the pool's current vault
/// balance, avoiding a duplicate RPC round trip.
pub async fn fetch_onchain_snapshot(
    rpc: &RpcClient,
    token: &TokenMeta,
    lp_mint: Option<Pubkey>,
    liquidity_sol: f64,
    now_ts: i64,
) -> Result<OnchainSnapshot, SafetyError> {
    let mint_account = rpc.get_account(&token.mint).await.map_err(|e| SafetyError::Rpc(e.to_string()))?;
    let mint_info = decode_mint_account(&mint_account.data)?;

    let top_holder_concentration_pct = if mint_info.supply > 0 {
        let largest = rpc
            .get_token_largest_accounts(&token.mint)
            .await
            .map_err(|e| SafetyError::Rpc(e.to_string()))?;
        let top_sum: u128 = largest.iter().filter_map(|a| a.amount.amount.parse::<u128>().ok()).sum();
        (top_sum as f64 / mint_info.supply as f64 * 100.0).min(100.0)
    } else {
        0.0
    };

    let lp_burned_or_locked = match (token.phase, lp_mint) {
        // Not applicable pre-graduation - the scorer skips this check for
        // bonding-curve tokens regardless of what we report here.
        (TokenPhase::BondingCurve, _) => true,
        // Migrated but we don't know the LP mint yet: fail closed rather
        // than assume it's fine.
        (TokenPhase::Migrated, None) => false,
        (TokenPhase::Migrated, Some(lp_mint)) => {
            let largest = rpc
                .get_token_largest_accounts(&lp_mint)
                .await
                .map_err(|e| SafetyError::Rpc(e.to_string()))?;
            let burn = burn_address().to_string();
            largest.first().map(|a| a.address == burn).unwrap_or(false)
        }
    };

    Ok(OnchainSnapshot {
        mint_authority_present: mint_info.mint_authority.is_some(),
        freeze_authority_present: mint_info.freeze_authority.is_some(),
        lp_burned_or_locked,
        top_holder_concentration_pct,
        liquidity_sol,
        age_secs: (now_ts - token.discovered_at).max(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression guard: the burn address is a hand-typed constant this
    /// module can't otherwise unit-test (everything else here needs a live
    /// RPC connection) - this at least catches a malformed/mistyped pubkey
    /// at test time instead of only failing silently at runtime.
    #[test]
    fn burn_address_parses_as_a_valid_pubkey() {
        let _ = burn_address();
    }
}
