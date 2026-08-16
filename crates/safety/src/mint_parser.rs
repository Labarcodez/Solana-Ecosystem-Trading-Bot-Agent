//! Decodes an SPL Token `Mint` account. Layout is the standard, stable
//! `spl_token::state::Mint` unpacked representation (82 bytes total,
//! unchanged since the SPL Token program's original release):
//!
//! ```text
//! offset  0..36  mint_authority   COption<Pubkey>  (4-byte tag + 32-byte pubkey)
//! offset 36..44  supply           u64 (LE)
//! offset 44      decimals         u8
//! offset 45      is_initialized   bool
//! offset 46..82  freeze_authority COption<Pubkey>
//! ```

use bot_core::Pubkey;

use crate::error::SafetyError;

pub const MINT_ACCOUNT_LEN: usize = 82;

#[derive(Debug, Clone)]
pub struct MintInfo {
    pub mint_authority: Option<Pubkey>,
    pub supply: u64,
    pub decimals: u8,
    pub freeze_authority: Option<Pubkey>,
}

pub fn decode_mint_account(data: &[u8]) -> Result<MintInfo, SafetyError> {
    if data.len() < MINT_ACCOUNT_LEN {
        return Err(SafetyError::ShortMintData { len: data.len() });
    }
    let mint_authority = decode_coption_pubkey(&data[0..36])?;
    let supply = u64::from_le_bytes(data[36..44].try_into().expect("length checked"));
    let decimals = data[44];
    let freeze_authority = decode_coption_pubkey(&data[46..82])?;
    Ok(MintInfo { mint_authority, supply, decimals, freeze_authority })
}

fn decode_coption_pubkey(bytes: &[u8]) -> Result<Option<Pubkey>, SafetyError> {
    let tag = u32::from_le_bytes(bytes[0..4].try_into().expect("length checked"));
    match tag {
        0 => Ok(None),
        1 => {
            let pk_bytes: [u8; 32] = bytes[4..36].try_into().expect("length checked");
            Ok(Some(Pubkey::from(pk_bytes)))
        }
        other => Err(SafetyError::InvalidCOption(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_mint_account(mint_authority: Option<Pubkey>, supply: u64, decimals: u8, freeze_authority: Option<Pubkey>) -> Vec<u8> {
        let mut data = vec![0u8; MINT_ACCOUNT_LEN];
        encode_coption(&mut data[0..36], mint_authority);
        data[36..44].copy_from_slice(&supply.to_le_bytes());
        data[44] = decimals;
        data[45] = 1; // is_initialized
        encode_coption(&mut data[46..82], freeze_authority);
        data
    }

    fn encode_coption(slot: &mut [u8], value: Option<Pubkey>) {
        match value {
            None => slot[0..4].copy_from_slice(&0u32.to_le_bytes()),
            Some(pk) => {
                slot[0..4].copy_from_slice(&1u32.to_le_bytes());
                slot[4..36].copy_from_slice(pk.as_ref());
            }
        }
    }

    #[test]
    fn decodes_revoked_authorities_as_none() {
        let data = fake_mint_account(None, 1_000_000, 6, None);
        let info = decode_mint_account(&data).unwrap();
        assert!(info.mint_authority.is_none());
        assert!(info.freeze_authority.is_none());
        assert_eq!(info.supply, 1_000_000);
        assert_eq!(info.decimals, 6);
    }

    #[test]
    fn decodes_present_authorities_as_some() {
        let mint_auth = Pubkey::new_unique();
        let freeze_auth = Pubkey::new_unique();
        let data = fake_mint_account(Some(mint_auth), 500, 9, Some(freeze_auth));
        let info = decode_mint_account(&data).unwrap();
        assert_eq!(info.mint_authority, Some(mint_auth));
        assert_eq!(info.freeze_authority, Some(freeze_auth));
    }

    #[test]
    fn rejects_short_data() {
        let short = vec![0u8; 40];
        assert!(matches!(decode_mint_account(&short), Err(SafetyError::ShortMintData { .. })));
    }

    #[test]
    fn rejects_invalid_coption_discriminant() {
        let mut data = fake_mint_account(None, 1, 0, None);
        data[0..4].copy_from_slice(&7u32.to_le_bytes()); // neither 0 nor 1
        assert!(matches!(decode_mint_account(&data), Err(SafetyError::InvalidCOption(7))));
    }
}
