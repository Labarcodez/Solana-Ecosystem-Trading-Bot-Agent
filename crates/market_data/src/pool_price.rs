//! Decodes an SPL Token account's raw `amount` field and turns two vault
//! balances into an approximate spot price. This works generically across
//! any constant-product-style AMM (Raydium, Orca, PumpSwap, ...) because it
//! only depends on the SPL Token program's account layout - stable across
//! the whole ecosystem - not on any individual DEX's proprietary pool
//! struct.

use crate::error::MarketDataError;

/// Byte offset of the `amount: u64` field within an `spl_token::state::Account`.
/// Layout: mint(32) + owner(32) + amount(8) + ... - fixed since SPL Token's
/// original release.
pub const SPL_TOKEN_ACCOUNT_AMOUNT_OFFSET: usize = 64;
/// Minimum length of a valid (unpacked) SPL Token account.
pub const SPL_TOKEN_ACCOUNT_MIN_LEN: usize = 165;

pub fn decode_token_account_amount(data: &[u8]) -> Result<u64, MarketDataError> {
    if data.len() < SPL_TOKEN_ACCOUNT_MIN_LEN {
        return Err(MarketDataError::ShortAccountData { len: data.len() });
    }
    let bytes: [u8; 8] = data[SPL_TOKEN_ACCOUNT_AMOUNT_OFFSET..SPL_TOKEN_ACCOUNT_AMOUNT_OFFSET + 8]
        .try_into()
        .expect("length checked above");
    Ok(u64::from_le_bytes(bytes))
}

/// Approximate spot price of one `base` token in `quote` units, from raw
/// (integer, pre-decimals) vault balances. This is a documented
/// simplification: it's the reserve ratio, not a true marginal price with
/// slippage/fees folded in - fine for a live price feed, not a substitute
/// for a real Jupiter quote at execution time.
pub fn price_from_vaults(
    base_raw: u64,
    base_decimals: u8,
    quote_raw: u64,
    quote_decimals: u8,
) -> Result<f64, MarketDataError> {
    if base_raw == 0 {
        return Err(MarketDataError::ZeroVaultBalance);
    }
    let base_ui = base_raw as f64 / 10f64.powi(base_decimals as i32);
    let quote_ui = quote_raw as f64 / 10f64.powi(quote_decimals as i32);
    Ok(quote_ui / base_ui)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_token_account(amount: u64) -> Vec<u8> {
        let mut data = vec![0u8; SPL_TOKEN_ACCOUNT_MIN_LEN];
        data[SPL_TOKEN_ACCOUNT_AMOUNT_OFFSET..SPL_TOKEN_ACCOUNT_AMOUNT_OFFSET + 8]
            .copy_from_slice(&amount.to_le_bytes());
        data
    }

    #[test]
    fn decodes_amount_at_the_correct_offset() {
        let data = fake_token_account(123_456_789);
        assert_eq!(decode_token_account_amount(&data).unwrap(), 123_456_789);
    }

    #[test]
    fn rejects_data_shorter_than_a_real_token_account() {
        let short = vec![0u8; 40];
        assert!(matches!(
            decode_token_account_amount(&short),
            Err(MarketDataError::ShortAccountData { .. })
        ));
    }

    #[test]
    fn price_from_vaults_is_the_reserve_ratio() {
        // 1,000 SOL (9 decimals) paired against 100,000 TOKEN (6 decimals).
        let quote_raw = 1_000 * 10u64.pow(9);
        let base_raw = 100_000 * 10u64.pow(6);
        let price = price_from_vaults(base_raw, 6, quote_raw, 9).unwrap();
        assert!((price - 0.01).abs() < 1e-9); // 1000 SOL / 100000 TOKEN = 0.01 SOL/TOKEN
    }

    #[test]
    fn zero_base_balance_is_a_clean_error_not_a_divide_by_zero_panic() {
        assert!(matches!(
            price_from_vaults(0, 6, 1000, 9),
            Err(MarketDataError::ZeroVaultBalance)
        ));
    }
}
