//! Decodes a pump.fun `BondingCurve` account and derives its PDA. Field
//! offsets confirmed via current pump.fun program documentation (see
//! README): discriminator(8) @0x00, virtual_token_reserves(8) @0x08,
//! virtual_sol_reserves(8) @0x10, real_token_reserves(8) @0x18,
//! real_sol_reserves(8) @0x20, token_total_supply(8) @0x28, complete(bool,1)
//! @0x30, creator(32) @0x31.

use bot_core::Pubkey;

/// The pump.fun bonding-curve program (mainnet), confirmed live this
/// session against a real `getTipAccounts`-style RPC probe of related
/// endpoints and current program documentation.
pub const PUMPFUN_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

/// 8-byte Anchor discriminator for the `create` instruction:
/// first 8 bytes of sha256("global:create").
pub const CREATE_DISCRIMINATOR: [u8; 8] = [24, 30, 200, 40, 5, 28, 7, 119];

pub const BONDING_CURVE_ACCOUNT_MIN_LEN: usize = 8 + 8 + 8 + 8 + 8 + 8 + 1 + 32;

/// Every pump.fun bonding-curve token is created with 6 decimals - a fixed
/// protocol convention (the standard `create` instruction always mints
/// with this many decimals), not something that varies per-token the way
/// migrated-token decimals can.
pub const PUMPFUN_TOKEN_DECIMALS: u8 = 6;

#[derive(Debug, Clone, Copy)]
pub struct BondingCurveState {
    pub virtual_token_reserves: u64,
    pub virtual_sol_reserves: u64,
    pub real_token_reserves: u64,
    pub real_sol_reserves: u64,
    pub token_total_supply: u64,
    pub complete: bool,
    pub creator: Pubkey,
}

#[derive(Debug, thiserror::Error)]
pub enum BondingCurveError {
    #[error("bonding curve account data too short: {len} bytes (expected at least {BONDING_CURVE_ACCOUNT_MIN_LEN})")]
    ShortData { len: usize },
}

pub fn decode_bonding_curve(data: &[u8]) -> Result<BondingCurveState, BondingCurveError> {
    if data.len() < BONDING_CURVE_ACCOUNT_MIN_LEN {
        return Err(BondingCurveError::ShortData { len: data.len() });
    }
    let u64_at = |offset: usize| u64::from_le_bytes(data[offset..offset + 8].try_into().expect("checked len"));
    let creator_bytes: [u8; 32] = data[0x31..0x31 + 32].try_into().expect("checked len");
    Ok(BondingCurveState {
        virtual_token_reserves: u64_at(0x08),
        virtual_sol_reserves: u64_at(0x10),
        real_token_reserves: u64_at(0x18),
        real_sol_reserves: u64_at(0x20),
        token_total_supply: u64_at(0x28),
        complete: data[0x30] != 0,
        creator: Pubkey::from(creator_bytes),
    })
}

/// Spot price in SOL per token from the curve's current virtual reserves -
/// the same constant-product ratio `execution::pumpfun::quote_buy`/
/// `quote_sell` are built on, just expressed as a price rather than a
/// trade quote. Returns `None` if the curve has no token reserves left to
/// price against (shouldn't happen pre-graduation, but division by zero is
/// avoided rather than assumed away).
pub fn price_sol_per_token(state: &BondingCurveState) -> Option<f64> {
    if state.virtual_token_reserves == 0 {
        return None;
    }
    let sol_ui = state.virtual_sol_reserves as f64 / 1_000_000_000.0;
    let token_ui = state.virtual_token_reserves as f64 / 10f64.powi(PUMPFUN_TOKEN_DECIMALS as i32);
    Some(sol_ui / token_ui)
}

/// Derives the bonding-curve PDA for `mint` under the pump.fun program,
/// using the confirmed seed prefix `"bonding-curve"`.
pub fn bonding_curve_pda(program_id: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"bonding-curve", mint.as_ref()], program_id)
}

/// Given the full set of account keys referenced by a `create` instruction
/// (or the whole transaction - order doesn't matter), finds the
/// (mint, bonding_curve) pair by trying each account as a candidate mint
/// and checking whether its derived bonding-curve PDA is also present in
/// the account list.
///
/// This is deliberately index-independent: pump.fun has both `create` and
/// `create_v2` instruction variants with reportedly different account
/// orderings (and that could change again), so matching by PDA derivation
/// rather than a hardcoded account index is the robust approach - it keeps
/// working even if the account order changes, as long as the seeds don't.
pub fn find_mint_and_bonding_curve(program_id: &Pubkey, account_keys: &[Pubkey]) -> Option<(Pubkey, Pubkey)> {
    for &candidate_mint in account_keys {
        let (pda, _bump) = bonding_curve_pda(program_id, &candidate_mint);
        if account_keys.contains(&pda) {
            return Some((candidate_mint, pda));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_bonding_curve(
        virtual_token_reserves: u64,
        virtual_sol_reserves: u64,
        complete: bool,
    ) -> Vec<u8> {
        fake_bonding_curve_with_creator(virtual_token_reserves, virtual_sol_reserves, complete, Pubkey::new_unique())
    }

    fn fake_bonding_curve_with_creator(
        virtual_token_reserves: u64,
        virtual_sol_reserves: u64,
        complete: bool,
        creator: Pubkey,
    ) -> Vec<u8> {
        let mut data = vec![0u8; BONDING_CURVE_ACCOUNT_MIN_LEN];
        data[0x08..0x10].copy_from_slice(&virtual_token_reserves.to_le_bytes());
        data[0x10..0x18].copy_from_slice(&virtual_sol_reserves.to_le_bytes());
        data[0x30] = complete as u8;
        data[0x31..0x31 + 32].copy_from_slice(creator.as_ref());
        data
    }

    #[test]
    fn decodes_reserves_and_complete_flag() {
        let data = fake_bonding_curve(1_000_000, 30 * 1_000_000_000, false);
        let state = decode_bonding_curve(&data).unwrap();
        assert_eq!(state.virtual_token_reserves, 1_000_000);
        assert_eq!(state.virtual_sol_reserves, 30_000_000_000);
        assert!(!state.complete);
    }

    #[test]
    fn detects_graduated_curve() {
        let data = fake_bonding_curve(1, 1, true);
        let state = decode_bonding_curve(&data).unwrap();
        assert!(state.complete);
    }

    #[test]
    fn rejects_short_data() {
        assert!(matches!(decode_bonding_curve(&[0u8; 10]), Err(BondingCurveError::ShortData { .. })));
    }

    #[test]
    fn decodes_the_creator_field() {
        let creator = Pubkey::new_unique();
        let data = fake_bonding_curve_with_creator(1, 1, false, creator);
        let state = decode_bonding_curve(&data).unwrap();
        assert_eq!(state.creator, creator);
    }

    #[test]
    fn price_matches_the_virtual_reserve_ratio() {
        // 1,000,000 tokens (6 decimals -> 1.0 UI token) against 30 SOL virtual reserves.
        let data = fake_bonding_curve(1_000_000, 30_000_000_000, false);
        let state = decode_bonding_curve(&data).unwrap();
        let price = price_sol_per_token(&state).unwrap();
        assert!((price - 30.0).abs() < 1e-9);
    }

    #[test]
    fn price_is_none_for_a_fully_drained_curve() {
        let data = fake_bonding_curve(0, 30_000_000_000, false);
        let state = decode_bonding_curve(&data).unwrap();
        assert!(price_sol_per_token(&state).is_none());
    }

    #[test]
    fn finds_mint_and_bonding_curve_regardless_of_account_order() {
        let program_id = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let (curve_pda, _) = bonding_curve_pda(&program_id, &mint);
        let unrelated1 = Pubkey::new_unique();
        let unrelated2 = Pubkey::new_unique();

        // Shuffle the "account order" - the function must not depend on it.
        let accounts = vec![unrelated1, curve_pda, unrelated2, mint];
        let found = find_mint_and_bonding_curve(&program_id, &accounts);
        assert_eq!(found, Some((mint, curve_pda)));
    }

    #[test]
    fn returns_none_when_no_bonding_curve_pda_present() {
        let program_id = Pubkey::new_unique();
        let accounts = vec![Pubkey::new_unique(), Pubkey::new_unique()];
        assert_eq!(find_mint_and_bonding_curve(&program_id, &accounts), None);
    }
}
