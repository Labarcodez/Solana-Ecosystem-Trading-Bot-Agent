//! Direct client for the pump.fun bonding-curve program - the only path
//! that can buy/sell a token still on its bonding curve (Jupiter does not
//! route these; confirmed via research this session). Hand-rolled since no
//! consistently-maintained crate exists for it.
//!
//! **Verification status (upgraded from "documentation-only" to
//! "IDL + live-transaction confirmed" in a follow-up pass):** the account
//! lists and PDA seeds below are taken directly from pump.fun's own public
//! Anchor IDL
//! (`https://raw.githubusercontent.com/pump-fun/pump-public-docs/main/idl/pump.json`,
//! `buy`/`sell` instructions), then cross-checked against two real, recent,
//! successful mainnet `Buy`/`Sell` transactions fetched via public RPC
//! (`solana-rpc.publicnode.com`) - the decoded account list from those
//! transactions matched the IDL account-for-account. This superseded an
//! earlier, incomplete 13-account guess assembled from secondary
//! documentation (confirmed wrong: real `buy` transactions use 16 accounts
//! with `buy` and `sell` using *different* orderings, not one shared list -
//! see `ordered_for_buy`/`ordered_for_sell` below).
//!
//! **Residual uncertainty, still worth knowing before trusting this at
//! scale:** (1) the exact Borsh wire encoding of `buy`'s third argument
//! (`track_volume: OptionBool`, an Anchor-custom optional-bool type) is
//! encoded here as a single `0x00` byte (the standard Borsh `None` encoding
//! for a 1-byte-tag Option) - this matches Anchor's usual `Option<T>`
//! convention but wasn't independently decoded byte-for-byte from a real
//! transaction's instruction data. (2) pump.fun mints can be either classic
//! SPL Token or Token-2022 (`token_program` differs) - callers must resolve
//! the mint's actual owning program and pass it to `PumpFunAccounts::derive`
//! rather than assuming classic SPL Token. (3) `decode_global_fee_recipient`'s
//! byte offset is taken from the IDL's `Global` struct field order only -
//! IDL-tier verification, not cross-checked against a live `Global` account
//! fetch (same caveat as `market_data::pumpswap_pool`).

use std::str::FromStr;

use bot_core::Pubkey;
use solana_sdk::instruction::{AccountMeta, Instruction};

use crate::error::ExecutionError;

pub const PUMPFUN_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
pub const PUMPFUN_FEE_PROGRAM_ID: &str = "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ";
pub const GLOBAL_CONFIG: &str = "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf";
pub const PUMPSWAP_PROGRAM_ID: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

/// Anchor discriminators, confirmed against the official IDL and matching
/// real transaction data byte-for-byte.
pub const BUY_DISCRIMINATOR: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
pub const SELL_DISCRIMINATOR: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

/// Anchor account discriminator for `Global` (first 8 bytes of
/// sha256("account:Global")), taken from the official IDL.
pub const GLOBAL_ACCOUNT_DISCRIMINATOR: [u8; 8] = [167, 232, 232, 177, 200, 108, 114, 127];

/// Byte offset of `fee_recipient` within a decoded `Global` account:
/// discriminator(8) + initialized(bool, 1) + authority(pubkey, 32) puts
/// `fee_recipient` at 41..73. Taken from the IDL's `Global` struct field
/// order, not independently cross-checked against a live account fetch in
/// this sandbox (no RPC key was available) - same verification tier as
/// `market_data::pumpswap_pool`, documented honestly rather than assumed.
const GLOBAL_FEE_RECIPIENT_OFFSET: usize = 8 + 1 + 32;
const GLOBAL_ACCOUNT_MIN_LEN: usize = GLOBAL_FEE_RECIPIENT_OFFSET + 32;

pub const ASSOCIATED_TOKEN_PROGRAM_ID: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
pub const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const TOKEN_2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
const SYSTEM_PROGRAM_ID: &str = "11111111111111111111111111111111";

/// The 32-byte const seed the IDL uses for `fee_config`'s second seed
/// component (a fixed value, not a per-mint one - taken verbatim from the
/// IDL's `pda.seeds` for `fee_config`).
const FEE_CONFIG_SEED_CONST: [u8; 32] = [
    1, 86, 224, 246, 147, 102, 90, 207, 68, 219, 21, 104, 191, 23, 91, 170, 81, 137, 203, 151, 245, 210, 255, 59,
    101, 93, 43, 182, 253, 109, 24, 176,
];

fn parse(addr: &str) -> Pubkey {
    Pubkey::from_str(addr).expect("valid static pubkey constant")
}

/// Bonding-curve PDA for `mint`, seeds `["bonding-curve", mint]` (IDL-confirmed).
pub fn bonding_curve_pda(mint: &Pubkey) -> Pubkey {
    let program_id = parse(PUMPFUN_PROGRAM_ID);
    Pubkey::find_program_address(&[b"bonding-curve", mint.as_ref()], &program_id).0
}

fn global_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"global"], program_id).0
}

/// Public accessor for the pump.fun `Global` config account's address -
/// what a caller needs to fetch it live (via RPC `getAccountInfo`) to decode
/// the current `fee_recipient` with [`decode_global_fee_recipient`].
pub fn global_config_pda() -> Pubkey {
    global_pda(&parse(PUMPFUN_PROGRAM_ID))
}

/// Decodes the current `fee_recipient` out of a fetched `Global` account's
/// raw data - this is the value `PumpFunAccounts::derive` needs and that
/// this module cannot hardcode (it can rotate; see module docs for why it's
/// not a static constant). Checks the account discriminator first so a
/// wrong-shaped account fails cleanly rather than yielding a garbage pubkey.
pub fn decode_global_fee_recipient(data: &[u8]) -> Result<Pubkey, ExecutionError> {
    if data.len() < GLOBAL_ACCOUNT_MIN_LEN {
        return Err(ExecutionError::UnexpectedResponse(format!(
            "Global account data too short: {} bytes (expected at least {GLOBAL_ACCOUNT_MIN_LEN})",
            data.len()
        )));
    }
    if data[0..8] != GLOBAL_ACCOUNT_DISCRIMINATOR {
        return Err(ExecutionError::UnexpectedResponse(
            "account data does not match the Global account discriminator".into(),
        ));
    }
    let bytes: [u8; 32] =
        data[GLOBAL_FEE_RECIPIENT_OFFSET..GLOBAL_FEE_RECIPIENT_OFFSET + 32].try_into().expect("length checked");
    Ok(Pubkey::from(bytes))
}

fn event_authority_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"__event_authority"], program_id).0
}

fn creator_vault_pda(program_id: &Pubkey, creator: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"creator-vault", creator.as_ref()], program_id).0
}

fn global_volume_accumulator_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"global_volume_accumulator"], program_id).0
}

fn user_volume_accumulator_pda(program_id: &Pubkey, user: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"user_volume_accumulator", user.as_ref()], program_id).0
}

fn fee_config_pda(fee_program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"fee_config", &FEE_CONFIG_SEED_CONST], fee_program).0
}

/// Standard SPL associated-token-account derivation, seeded by
/// `[owner, token_program, mint]` under the associated-token program - not
/// pump.fun-specific, this rule is fixed ecosystem-wide. `token_program`
/// must be whichever program actually owns `mint` (classic SPL Token or
/// Token-2022) - the ATA address differs depending on which.
pub fn associated_token_account(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    let ata_program = parse(ASSOCIATED_TOKEN_PROGRAM_ID);
    Pubkey::find_program_address(&[owner.as_ref(), token_program.as_ref(), mint.as_ref()], &ata_program).0
}

/// Every account a `buy`/`sell` instruction might need. `buy` and `sell`
/// use *different* orderings of (mostly) this same set - see
/// `ordered_for_buy`/`ordered_for_sell`, both IDL-confirmed.
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
    pub creator_vault: Pubkey,
    pub event_authority: Pubkey,
    pub program: Pubkey,
    pub global_volume_accumulator: Pubkey,
    pub user_volume_accumulator: Pubkey,
    pub fee_config: Pubkey,
    pub fee_program: Pubkey,
}

impl PumpFunAccounts {
    /// Derives every account pump.fun's IDL names for `buy`/`sell`, given
    /// `mint`, the trading `user`, the token's `creator` (from the
    /// bonding-curve account's `creator` field - see
    /// `discovery::bonding_curve::BondingCurveState::creator`), the current
    /// `fee_recipient` (from the `global` config account, or observed from
    /// a recent real transaction), and `token_program` (classic SPL Token
    /// or Token-2022 - whichever actually owns `mint`).
    pub fn derive(
        mint: Pubkey,
        user: Pubkey,
        creator: Pubkey,
        fee_recipient: Pubkey,
        token_program: Pubkey,
    ) -> Self {
        let program = parse(PUMPFUN_PROGRAM_ID);
        let fee_program = parse(PUMPFUN_FEE_PROGRAM_ID);
        let bonding_curve = bonding_curve_pda(&mint);
        Self {
            global: global_pda(&program),
            fee_recipient,
            mint,
            bonding_curve,
            associated_bonding_curve: associated_token_account(&bonding_curve, &mint, &token_program),
            associated_user: associated_token_account(&user, &mint, &token_program),
            user,
            system_program: parse(SYSTEM_PROGRAM_ID),
            token_program,
            creator_vault: creator_vault_pda(&program, &creator),
            event_authority: event_authority_pda(&program),
            program,
            global_volume_accumulator: global_volume_accumulator_pda(&program),
            user_volume_accumulator: user_volume_accumulator_pda(&program, &user),
            fee_config: fee_config_pda(&fee_program),
            fee_program,
        }
    }

    /// IDL-confirmed 16-account order for `buy`.
    pub fn ordered_for_buy(&self) -> Vec<Pubkey> {
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
            self.global_volume_accumulator,
            self.user_volume_accumulator,
            self.fee_config,
            self.fee_program,
        ]
    }

    /// IDL-confirmed 14-account order for `sell` - note `token_program`
    /// moves to after `creator_vault` (not before, as in `buy`), and there
    /// are no volume-accumulator accounts at all.
    pub fn ordered_for_sell(&self) -> Vec<Pubkey> {
        vec![
            self.global,
            self.fee_recipient,
            self.mint,
            self.bonding_curve,
            self.associated_bonding_curve,
            self.associated_user,
            self.user,
            self.system_program,
            self.creator_vault,
            self.token_program,
            self.event_authority,
            self.program,
            self.fee_config,
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

/// Builds the raw instruction data for `buy`: `amount` (min token amount
/// out, after slippage), `max_sol_cost` (lamports), and `track_volume`
/// (Anchor `OptionBool`, encoded here as Borsh `None` - see module docs for
/// the residual uncertainty on this one field).
pub fn buy_instruction_data(token_amount_out_min: u64, max_sol_cost: u64) -> Vec<u8> {
    let mut data = BUY_DISCRIMINATOR.to_vec();
    data.extend_from_slice(&token_amount_out_min.to_le_bytes());
    data.extend_from_slice(&max_sol_cost.to_le_bytes());
    data.push(0); // track_volume: None
    data
}

/// Builds the raw instruction data for `sell`: `amount` (token amount in)
/// and `min_sol_output` (lamports, after slippage). Unlike `buy`, `sell`
/// takes no third argument (IDL-confirmed).
pub fn sell_instruction_data(token_amount_in: u64, min_sol_output: u64) -> Vec<u8> {
    let mut data = SELL_DISCRIMINATOR.to_vec();
    data.extend_from_slice(&token_amount_in.to_le_bytes());
    data.extend_from_slice(&min_sol_output.to_le_bytes());
    data
}

/// A real `Instruction`, ready to include in a transaction, for a `buy`.
/// Account writable/signer flags are taken directly from the IDL (see
/// module docs).
pub fn buy_instruction(accounts: &PumpFunAccounts, token_amount_out_min: u64, max_sol_cost: u64) -> Instruction {
    let a = accounts;
    let metas = vec![
        AccountMeta::new_readonly(a.global, false),
        AccountMeta::new(a.fee_recipient, false),
        AccountMeta::new_readonly(a.mint, false),
        AccountMeta::new(a.bonding_curve, false),
        AccountMeta::new(a.associated_bonding_curve, false),
        AccountMeta::new(a.associated_user, false),
        AccountMeta::new(a.user, true),
        AccountMeta::new_readonly(a.system_program, false),
        AccountMeta::new_readonly(a.token_program, false),
        AccountMeta::new(a.creator_vault, false),
        AccountMeta::new_readonly(a.event_authority, false),
        AccountMeta::new_readonly(a.program, false),
        AccountMeta::new_readonly(a.global_volume_accumulator, false),
        AccountMeta::new(a.user_volume_accumulator, false),
        AccountMeta::new_readonly(a.fee_config, false),
        AccountMeta::new_readonly(a.fee_program, false),
    ];
    Instruction { program_id: a.program, accounts: metas, data: buy_instruction_data(token_amount_out_min, max_sol_cost) }
}

/// A real `Instruction`, ready to include in a transaction, for a `sell`.
pub fn sell_instruction(accounts: &PumpFunAccounts, token_amount_in: u64, min_sol_output: u64) -> Instruction {
    let a = accounts;
    let metas = vec![
        AccountMeta::new_readonly(a.global, false),
        AccountMeta::new(a.fee_recipient, false),
        AccountMeta::new_readonly(a.mint, false),
        AccountMeta::new(a.bonding_curve, false),
        AccountMeta::new(a.associated_bonding_curve, false),
        AccountMeta::new(a.associated_user, false),
        AccountMeta::new(a.user, true),
        AccountMeta::new_readonly(a.system_program, false),
        AccountMeta::new(a.creator_vault, false),
        AccountMeta::new_readonly(a.token_program, false),
        AccountMeta::new_readonly(a.event_authority, false),
        AccountMeta::new_readonly(a.program, false),
        AccountMeta::new_readonly(a.fee_config, false),
        AccountMeta::new_readonly(a.fee_program, false),
    ];
    Instruction { program_id: a.program, accounts: metas, data: sell_instruction_data(token_amount_in, min_sol_output) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classic_token_program() -> Pubkey {
        parse(TOKEN_PROGRAM_ID)
    }

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
    fn buy_instruction_data_has_correct_discriminator_and_length() {
        let buy = buy_instruction_data(1000, 2000);
        assert_eq!(&buy[0..8], &BUY_DISCRIMINATOR);
        // discriminator(8) + amount(8) + max_sol_cost(8) + track_volume(1)
        assert_eq!(buy.len(), 8 + 8 + 8 + 1);
        assert_eq!(buy[24], 0, "track_volume should encode as None (0x00)");
    }

    #[test]
    fn sell_instruction_data_has_correct_discriminator_and_length() {
        let sell = sell_instruction_data(1000, 2000);
        assert_eq!(&sell[0..8], &SELL_DISCRIMINATOR);
        // sell takes no third argument, unlike buy
        assert_eq!(sell.len(), 8 + 8 + 8);
    }

    #[test]
    fn derived_accounts_are_deterministic() {
        let mint = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let creator = Pubkey::new_unique();
        let fee_recipient = Pubkey::new_unique();
        let tp = classic_token_program();
        let a1 = PumpFunAccounts::derive(mint, user, creator, fee_recipient, tp);
        let a2 = PumpFunAccounts::derive(mint, user, creator, fee_recipient, tp);
        assert_eq!(a1.ordered_for_buy(), a2.ordered_for_buy(), "derivation must be deterministic");
        assert_eq!(a1.bonding_curve, bonding_curve_pda(&mint));
    }

    #[test]
    fn buy_and_sell_orderings_differ_in_length_and_token_program_position() {
        let mint = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let creator = Pubkey::new_unique();
        let fee_recipient = Pubkey::new_unique();
        let accounts = PumpFunAccounts::derive(mint, user, creator, fee_recipient, classic_token_program());

        let buy_order = accounts.ordered_for_buy();
        let sell_order = accounts.ordered_for_sell();
        assert_eq!(buy_order.len(), 16);
        assert_eq!(sell_order.len(), 14);

        // token_program sits at index 8 in buy (before creator_vault) but
        // index 9 in sell (after creator_vault) - a real, IDL-confirmed
        // difference between the two instructions, not a copy-paste slot.
        assert_eq!(buy_order[8], accounts.token_program);
        assert_eq!(sell_order[9], accounts.token_program);
        assert_eq!(sell_order[8], accounts.creator_vault);
    }

    #[test]
    fn different_token_programs_change_the_associated_token_accounts() {
        let mint = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let creator = Pubkey::new_unique();
        let fee_recipient = Pubkey::new_unique();
        let classic = PumpFunAccounts::derive(mint, user, creator, fee_recipient, classic_token_program());
        let token2022 = PumpFunAccounts::derive(mint, user, creator, fee_recipient, parse(TOKEN_2022_PROGRAM_ID));
        assert_ne!(classic.associated_user, token2022.associated_user);
        assert_ne!(classic.associated_bonding_curve, token2022.associated_bonding_curve);
        // Accounts independent of token_program must stay identical.
        assert_eq!(classic.bonding_curve, token2022.bonding_curve);
        assert_eq!(classic.global, token2022.global);
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
            TOKEN_2022_PROGRAM_ID,
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

    #[test]
    fn buy_instruction_accounts_match_ordered_for_buy_exactly() {
        let mint = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let accounts = PumpFunAccounts::derive(mint, user, Pubkey::new_unique(), Pubkey::new_unique(), classic_token_program());
        let ix = buy_instruction(&accounts, 100, 200);
        assert_eq!(ix.program_id, accounts.program);
        let ix_accounts: Vec<Pubkey> = ix.accounts.iter().map(|m| m.pubkey).collect();
        assert_eq!(ix_accounts, accounts.ordered_for_buy());
        // The one signer must be `user`, at the position the IDL specifies.
        assert!(ix.accounts[6].is_signer);
        assert_eq!(ix.accounts.iter().filter(|m| m.is_signer).count(), 1);
    }

    #[test]
    fn sell_instruction_accounts_match_ordered_for_sell_exactly() {
        let mint = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let accounts = PumpFunAccounts::derive(mint, user, Pubkey::new_unique(), Pubkey::new_unique(), classic_token_program());
        let ix = sell_instruction(&accounts, 100, 200);
        assert_eq!(ix.program_id, accounts.program);
        let ix_accounts: Vec<Pubkey> = ix.accounts.iter().map(|m| m.pubkey).collect();
        assert_eq!(ix_accounts, accounts.ordered_for_sell());
        assert!(ix.accounts[6].is_signer);
    }

    /// Cross-check against real data: `event_authority` and
    /// `global_volume_accumulator` were observed as fixed addresses across
    /// multiple independent real mainnet transactions this session
    /// (`Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1` and
    /// `Hq2wp8uJ9jCPsYgNHex8RtqdvMPfVGoYwjvF1ATiwn2Y` respectively) -
    /// confirming they're global PDAs with no per-mint/per-user component,
    /// matching what their IDL seed lists say (`const` seeds only).
    fn fake_global_account(fee_recipient: Pubkey) -> Vec<u8> {
        let mut data = vec![0u8; GLOBAL_ACCOUNT_MIN_LEN];
        data[0..8].copy_from_slice(&GLOBAL_ACCOUNT_DISCRIMINATOR);
        data[GLOBAL_FEE_RECIPIENT_OFFSET..GLOBAL_FEE_RECIPIENT_OFFSET + 32].copy_from_slice(fee_recipient.as_ref());
        data
    }

    #[test]
    fn decodes_fee_recipient_at_the_right_offset() {
        let fee_recipient = Pubkey::new_unique();
        let data = fake_global_account(fee_recipient);
        assert_eq!(decode_global_fee_recipient(&data).unwrap(), fee_recipient);
    }

    #[test]
    fn rejects_global_account_data_with_the_wrong_discriminator() {
        let mut data = fake_global_account(Pubkey::new_unique());
        data[0] ^= 0xFF;
        assert!(decode_global_fee_recipient(&data).is_err());
    }

    #[test]
    fn rejects_short_global_account_data() {
        assert!(decode_global_fee_recipient(&[0u8; 10]).is_err());
    }

    #[test]
    fn global_config_pda_matches_manual_derivation() {
        let program = parse(PUMPFUN_PROGRAM_ID);
        assert_eq!(global_config_pda(), global_pda(&program));
    }

    #[test]
    fn global_pdas_match_addresses_observed_in_real_transactions() {
        let program = parse(PUMPFUN_PROGRAM_ID);
        assert_eq!(event_authority_pda(&program).to_string(), "Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1");
        assert_eq!(
            global_volume_accumulator_pda(&program).to_string(),
            "Hq2wp8uJ9jCPsYgNHex8RtqdvMPfVGoYwjvF1ATiwn2Y"
        );
    }
}
