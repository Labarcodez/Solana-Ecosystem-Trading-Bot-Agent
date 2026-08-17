//! Decodes a PumpSwap `Pool` account to get the vault (token account)
//! addresses `pool_price`'s generic vault-ratio pricing needs. This is what
//! lets a migrated (post-graduation) token's price be watched without
//! needing to decode each DEX's own swap instruction - once we know the
//! two vault addresses, `pool_price::price_from_vaults` works exactly as
//! it does for any other constant-product-style pool.
//!
//! **Verification tier (documented honestly, matching this session's
//! practice elsewhere):** the account layout and discriminator below are
//! taken directly from PumpSwap's official Anchor IDL
//! (`https://raw.githubusercontent.com/pump-fun/pump-public-docs/main/idl/pump_amm.json`,
//! the `Pool` account type). Unlike `execution::pumpfun`'s bonding-curve
//! accounts, this was **not** independently cross-checked against a live
//! transaction's raw bytes in this session - fetching one hit Solana's
//! multi-address-lookup-table account resolution being ambiguous over the
//! public RPC endpoint available here (no funded RPC key was available to
//! use a provider with clean ALT resolution). The 8-byte account
//! discriminator check in `decode_pool_account` is the safety net: a
//! wrong-shaped account fails to decode rather than silently returning
//! garbage vault addresses.

use bot_core::Pubkey;

use crate::error::MarketDataError;

pub const PUMPSWAP_PROGRAM_ID: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

/// Anchor account discriminator for `Pool` (first 8 bytes of
/// sha256("account:Pool")), taken from the IDL.
pub const POOL_ACCOUNT_DISCRIMINATOR: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188];

/// Minimum byte length of a `Pool` account. Field sizes, in order:
/// discriminator 8, pool_bump 1, index 2, creator 32, base_mint 32,
/// quote_mint 32, lp_mint 32, pool_base_token_account 32,
/// pool_quote_token_account 32, lp_supply 8, coin_creator 32,
/// is_mayhem_mode 1, is_cashback_coin 1, virtual_quote_reserves 16.
pub const POOL_ACCOUNT_MIN_LEN: usize = 8 + 1 + 2 + 32 + 32 + 32 + 32 + 32 + 32 + 8 + 32 + 1 + 1 + 16;

#[derive(Debug, Clone, Copy)]
pub struct PumpSwapPool {
    pub creator: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub lp_mint: Pubkey,
    pub pool_base_token_account: Pubkey,
    pub pool_quote_token_account: Pubkey,
}

pub fn decode_pool_account(data: &[u8]) -> Result<PumpSwapPool, MarketDataError> {
    if data.len() < POOL_ACCOUNT_MIN_LEN {
        return Err(MarketDataError::ShortAccountData { len: data.len() });
    }
    if data[0..8] != POOL_ACCOUNT_DISCRIMINATOR {
        return Err(MarketDataError::WrongAccountType);
    }
    let pubkey_at = |offset: usize| {
        let bytes: [u8; 32] = data[offset..offset + 32].try_into().expect("length checked");
        Pubkey::from(bytes)
    };
    Ok(PumpSwapPool {
        creator: pubkey_at(11),
        base_mint: pubkey_at(43),
        quote_mint: pubkey_at(75),
        lp_mint: pubkey_at(107),
        pool_base_token_account: pubkey_at(139),
        pool_quote_token_account: pubkey_at(171),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_pool_account(
        creator: Pubkey,
        base_mint: Pubkey,
        quote_mint: Pubkey,
        lp_mint: Pubkey,
        pool_base: Pubkey,
        pool_quote: Pubkey,
    ) -> Vec<u8> {
        let mut data = vec![0u8; POOL_ACCOUNT_MIN_LEN];
        data[0..8].copy_from_slice(&POOL_ACCOUNT_DISCRIMINATOR);
        data[11..43].copy_from_slice(creator.as_ref());
        data[43..75].copy_from_slice(base_mint.as_ref());
        data[75..107].copy_from_slice(quote_mint.as_ref());
        data[107..139].copy_from_slice(lp_mint.as_ref());
        data[139..171].copy_from_slice(pool_base.as_ref());
        data[171..203].copy_from_slice(pool_quote.as_ref());
        data
    }

    #[test]
    fn decodes_all_pubkey_fields_at_the_right_offsets() {
        let creator = Pubkey::new_unique();
        let base_mint = Pubkey::new_unique();
        let quote_mint = Pubkey::new_unique();
        let lp_mint = Pubkey::new_unique();
        let pool_base = Pubkey::new_unique();
        let pool_quote = Pubkey::new_unique();
        let data = fake_pool_account(creator, base_mint, quote_mint, lp_mint, pool_base, pool_quote);

        let pool = decode_pool_account(&data).unwrap();
        assert_eq!(pool.creator, creator);
        assert_eq!(pool.base_mint, base_mint);
        assert_eq!(pool.quote_mint, quote_mint);
        assert_eq!(pool.lp_mint, lp_mint);
        assert_eq!(pool.pool_base_token_account, pool_base);
        assert_eq!(pool.pool_quote_token_account, pool_quote);
    }

    #[test]
    fn rejects_data_with_the_wrong_discriminator() {
        let mut data = fake_pool_account(
            Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(),
            Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(),
        );
        data[0] ^= 0xFF; // corrupt the discriminator
        assert!(matches!(decode_pool_account(&data), Err(MarketDataError::WrongAccountType)));
    }

    #[test]
    fn rejects_short_data() {
        assert!(matches!(decode_pool_account(&[0u8; 50]), Err(MarketDataError::ShortAccountData { .. })));
    }

    #[test]
    fn distinguishes_pool_accounts_from_other_pumpswap_owned_accounts() {
        // Any other PumpSwap account type (GlobalConfig, FeeConfig, ...)
        // sharing the same owner-filter subscription must be rejected, not
        // mis-decoded as a Pool.
        let mut data = vec![0u8; POOL_ACCOUNT_MIN_LEN];
        data[0..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]); // some other account's discriminator
        assert!(matches!(decode_pool_account(&data), Err(MarketDataError::WrongAccountType)));
    }
}
