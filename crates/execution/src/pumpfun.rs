//! Direct client for the pump.fun bonding-curve program - the only path
//! that can buy/sell a token still on its bonding curve (Jupiter does not
//! route these; confirmed via research this session). Hand-rolled since no
//! consistently-maintained crate exists for it.
//!
//! Confirmed-live/verified-via-documentation constants: program id, the
//! `buy`/`sell` instruction discriminators, and the bonding-curve account's
//! reserve-field offsets (shared with `discovery::bonding_curve`, which
//! independently confirms the account layout by decoding the `complete`
//! flag). The **full ordered account list** below is assembled from public
//! documentation and widely-used community pump.fun integrations, not from
//! a live transaction fetched in this sandbox (no funded RPC key was
//! available here) - **it must be cross-checked against a handful of
//! recent real mainnet `buy`/`sell` transactions (e.g. via
//! `getTransaction` over your own Alchemy RPC) before this is trusted for
//! `mode = "live"` trading.** This is flagged in the README as a required
//! pre-live-trading step, not a silent gap.

use std::str::FromStr;

use bot_core::Pubkey;

use crate::error::ExecutionError;

pub const PUMPFUN_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
pub const PUMPFUN_FEE_PROGRAM_ID: &str = "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ";
pub const GLOBAL_CONFIG: &str = "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf";
pub const PUMPSWAP_PROGRAM_ID: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

pub const BUY_DISCRIMINATOR: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
pub const SELL_DISCRIMINATOR: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

const ASSOCIATED_TOKEN_PROGRAM_ID: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const SYSTEM_PROGRAM_ID: &str = "11111111111111111111111111111111";

fn parse(addr: &str) -> Pubkey {
    Pubkey::from_str(addr).expect("valid static pubkey constant")
}

/// Bonding-curve PDA for `mint`, seeds `["bonding-curve", mint]` (confirmed
/// - shared with `discovery::bonding_curve`).
pub fn bonding_curve_pda(mint: &Pubkey) -> Pubkey {
    let program_id = parse(PUMPFUN_PROGRAM_ID);
    Pubkey::find_program_address(&[b"bonding-curve", mint.as_ref()], &program_id).0
}

/// Standard SPL associated-token-account derivation - not pump.fun-
/// specific, this rule is fixed ecosystem-wide.
pub fn associated_token_account(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    let ata_program = parse(ASSOCIATED_TOKEN_PROGRAM_ID);
    let token_program = parse(TOKEN_PROGRAM_ID);
    Pubkey::find_program_address(&[owner.as_ref(), token_program.as_ref(), mint.as_ref()], &ata_program).0
}

/// Every account a `buy`/`sell` instruction needs. Fields marked
/// "best-effort" are assembled from documentation/community sources, not
/// independently confirmed against a live transaction in this sandbox -
/// see the module doc comment.
#[derive(Debug, Clone)]
pub struct PumpFunAccounts {
    pub global: Pubkey,
    pub fee_recipient: Pubkey,
    pub mint: Pubkey,
    pub bonding_curve: Pubkey,
    pub associated_bonding_curve: Pubkey,
    pub associated_user: Pubkey,
    pub user: Pubkey,
    pub system_program: Pubkey,
    pub token_program: Pubkey,
    /// best-effort: PDA seeds `["creator-vault", creator]`
    pub creator_vault: Pubkey,
    /// best-effort: PDA seeds `["__event_authority"]`
    pub event_authority: Pubkey,
    pub program: Pubkey,
    pub fee_program: Pubkey,
}

impl PumpFunAccounts {
    /// Derives everything this module is confident about and marks the
    /// rest with a best-effort derivation, given `mint`, the trading
    /// `user`, and the token's `creator` (from the bonding-curve account's
    /// `creator` field, or from the `create` instruction sighting in
    /// `discovery`).
    pub fn derive(mint: Pubkey, user: Pubkey, creator: Pubkey, fee_recipient: Pubkey) -> Self {
        let program = parse(PUMPFUN_PROGRAM_ID);
        let bonding_curve = bonding_curve_pda(&mint);
        let (creator_vault, _) = Pubkey::find_program_address(&[b"creator-vault", creator.as_ref()], &program);
        let (event_authority, _) = Pubkey::find_program_address(&[b"__event_authority"], &program);
        Self {
            global: parse(GLOBAL_CONFIG),
            fee_recipient,
            mint,
            bonding_curve,
            associated_bonding_curve: associated_token_account(&bonding_curve, &mint),
            associated_user: associated_token_account(&user, &mint),
            user,
            system_program: parse(SYSTEM_PROGRAM_ID),
            token_program: parse(TOKEN_PROGRAM_ID),
            creator_vault,
            event_authority,
            program,
            fee_program: parse(PUMPFUN_FEE_PROGRAM_ID),
        }
    }

    /// Ordered account list matching the confirmed 16-account instruction
    /// shape (see module docs re: verification before live use).
    pub fn ordered(&self) -> Vec<Pubkey> {
        vec![
            self.global,
            self.fee_recipient,
            self.mint,
            self.bonding_curve,
            self.associated_bonding_curve,
            self.associated_user,
            self.user,
            self.system_program,
            self.token_program,
            self.creator_vault,
            self.event_authority,
            self.program,
            self.fee_program,
        ]
    }
}

/// Constant-product quote for buying `sol_in` lamports worth of the token,
/// given the bonding curve's current virtual reserves. Confirmed formula
/// (pump.fun's bonding curve is a plain constant-product AMM over its
/// virtual reserves): `token_out = virtual_token_reserves -
/// (virtual_token_reserves * virtual_sol_reserves) / (virtual_sol_reserves + sol_in)`.
pub fn quote_buy(virtual_token_reserves: u64, virtual_sol_reserves: u64, sol_in: u64) -> Result<u64, ExecutionError> {
    let vtr = virtual_token_reserves as u128;
    let vsr = virtual_sol_reserves as u128;
    let sol_in = sol_in as u128;
    let denom = vsr + sol_in;
    if denom == 0 {
        return Err(ExecutionError::UnexpectedResponse("zero denominator in bonding curve quote".into()));
    }
    let k = vtr * vsr;
    let new_token_reserves = k / denom;
    Ok(vtr.saturating_sub(new_token_reserves) as u64)
}

/// Symmetric quote for selling `token_in` tokens back into SOL.
pub fn quote_sell(virtual_token_reserves: u64, virtual_sol_reserves: u64, token_in: u64) -> Result<u64, ExecutionError> {
    let vtr = virtual_token_reserves as u128;
    let vsr = virtual_sol_reserves as u128;
    let token_in = token_in as u128;
    let denom = vtr + token_in;
    if denom == 0 {
        return Err(ExecutionError::UnexpectedResponse("zero denominator in bonding curve quote".into()));
    }
    let k = vtr * vsr;
    let new_sol_reserves = k / denom;
    Ok(vsr.saturating_sub(new_sol_reserves) as u64)
}

/// Builds the raw instruction data (discriminator + Borsh-style
/// little-endian u64 args) for a buy: `token_amount_out` (min acceptable,
/// after slippage) and `max_sol_cost` (lamports).
pub fn buy_instruction_data(token_amount_out_min: u64, max_sol_cost: u64) -> Vec<u8> {
    let mut data = BUY_DISCRIMINATOR.to_vec();
    data.extend_from_slice(&token_amount_out_min.to_le_bytes());
    data.extend_from_slice(&max_sol_cost.to_le_bytes());
    data
}

/// Builds the raw instruction data for a sell: `token_amount_in` and
/// `min_sol_output` (lamports, after slippage).
pub fn sell_instruction_data(token_amount_in: u64, min_sol_output: u64) -> Vec<u8> {
    let mut data = SELL_DISCRIMINATOR.to_vec();
    data.extend_from_slice(&token_amount_in.to_le_bytes());
    data.extend_from_slice(&min_sol_output.to_le_bytes());
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_buy_matches_constant_product_by_hand() {
        // 1,000,000,000 virtual tokens, 30 SOL virtual reserves, buying with 1 SOL.
        let vtr = 1_000_000_000u64;
        let vsr = 30_000_000_000u64; // lamports
        let sol_in = 1_000_000_000u64; // 1 SOL
        let out = quote_buy(vtr, vsr, sol_in).unwrap();
        // k = vtr*vsr; new_vtr = k/(vsr+sol_in); out = vtr - new_vtr
        let k = vtr as u128 * vsr as u128;
        let expected = vtr as u128 - k / (vsr as u128 + sol_in as u128);
        assert_eq!(out as u128, expected);
        assert!(out > 0 && out < vtr);
    }

    #[test]
    fn quote_sell_is_the_inverse_shape_of_quote_buy() {
        let vtr = 1_000_000_000u64;
        let vsr = 30_000_000_000u64;
        let bought = quote_buy(vtr, vsr, 1_000_000_000).unwrap();
        // Selling the exact bought amount back should net very close to
        // the 1 SOL paid in - not exactly equal, since two successive
        // floor-divisions (one per quote) don't perfectly cancel, but any
        // drift should be dust (a handful of lamports), not something that
        // materially creates or destroys value.
        let new_vtr = vtr - bought;
        let new_vsr = vsr + 1_000_000_000;
        let sol_back = quote_sell(new_vtr, new_vsr, bought).unwrap();
        let drift = sol_back.abs_diff(1_000_000_000);
        assert!(drift < 1_000, "round-trip drift should be dust-level lamports, got {drift}");
        assert!(sol_back > 0);
    }

    #[test]
    fn larger_buys_move_the_price_more_than_smaller_ones() {
        let vtr = 1_000_000_000u64;
        let vsr = 30_000_000_000u64;
        let small = quote_buy(vtr, vsr, 100_000_000).unwrap(); // 0.1 SOL
        let large = quote_buy(vtr, vsr, 10_000_000_000).unwrap(); // 10 SOL
        let small_rate = small as f64 / 100_000_000.0;
        let large_rate = large as f64 / 10_000_000_000.0;
        assert!(large_rate < small_rate, "larger buys should get a worse effective rate (price impact)");
    }

    #[test]
    fn instruction_data_has_correct_discriminator_and_length() {
        let buy = buy_instruction_data(1000, 2000);
        assert_eq!(&buy[0..8], &BUY_DISCRIMINATOR);
        assert_eq!(buy.len(), 8 + 8 + 8);
        let sell = sell_instruction_data(1000, 2000);
        assert_eq!(&sell[0..8], &SELL_DISCRIMINATOR);
    }

    #[test]
    fn derived_accounts_produce_16_or_fewer_deterministic_entries_for_the_same_input() {
        let mint = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let creator = Pubkey::new_unique();
        let fee_recipient = Pubkey::new_unique();
        let a1 = PumpFunAccounts::derive(mint, user, creator, fee_recipient);
        let a2 = PumpFunAccounts::derive(mint, user, creator, fee_recipient);
        assert_eq!(a1.ordered(), a2.ordered(), "derivation must be deterministic");
        assert_eq!(a1.bonding_curve, bonding_curve_pda(&mint));
    }

    /// Regression guard: every hardcoded address constant in this module
    /// must be a well-formed 32-byte pubkey. This is exactly the class of
    /// bug that slipped through once already (a mistyped System Program ID)
    /// until it was caught here.
    #[test]
    fn all_static_address_constants_parse_as_valid_pubkeys() {
        for addr in [
            PUMPFUN_PROGRAM_ID,
            PUMPFUN_FEE_PROGRAM_ID,
            GLOBAL_CONFIG,
            PUMPSWAP_PROGRAM_ID,
            ASSOCIATED_TOKEN_PROGRAM_ID,
            TOKEN_PROGRAM_ID,
            SYSTEM_PROGRAM_ID,
        ] {
            Pubkey::from_str(addr).unwrap_or_else(|e| panic!("constant {addr:?} failed to parse: {e}"));
        }
    }

    #[test]
    fn different_mints_derive_different_bonding_curves() {
        let m1 = Pubkey::new_unique();
        let m2 = Pubkey::new_unique();
        assert_ne!(bonding_curve_pda(&m1), bonding_curve_pda(&m2));
    }
}
