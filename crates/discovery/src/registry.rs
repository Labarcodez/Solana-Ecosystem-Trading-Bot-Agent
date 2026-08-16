//! In-memory `mint -> TokenMeta` registry, plus a `bonding_curve_pda ->
//! mint` reverse index (a graduation account update only carries the
//! curve's own pubkey, not the mint it belongs to - the reverse index is
//! how that gets resolved back). Pure logic, no I/O, which is what keeps
//! this fully unit-testable without a gRPC connection.

use std::collections::HashMap;

use bot_core::{Pubkey, TokenMeta, TokenPhase, TrustTier};

#[derive(Debug, Clone, PartialEq)]
pub enum DiscoveryEvent {
    /// A brand-new pump.fun token was just created (still on the bonding curve).
    TokenDiscovered(TokenMeta),
    /// A previously-discovered token's bonding curve just completed
    /// (graduated to an AMM pool).
    TokenGraduated { mint: Pubkey, ts: i64 },
}

#[derive(Default)]
pub struct MintRegistry {
    tokens: HashMap<Pubkey, TokenMeta>,
    curve_to_mint: HashMap<Pubkey, Pubkey>,
}

impl MintRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, mint: &Pubkey) -> Option<&TokenMeta> {
        self.tokens.get(mint)
    }

    pub fn resolve_curve(&self, curve_pda: &Pubkey) -> Option<Pubkey> {
        self.curve_to_mint.get(curve_pda).copied()
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Registers a newly-created pump.fun token and its bonding-curve PDA.
    /// Returns `None` (no event) if this mint is already known - discovery
    /// is idempotent, a duplicate `create` sighting (e.g. after a gRPC
    /// reconnect) shouldn't re-fire downstream safety scoring from scratch.
    pub fn observe_created(&mut self, mint: Pubkey, curve_pda: Pubkey, ts: i64, source: &str) -> Option<DiscoveryEvent> {
        if self.tokens.contains_key(&mint) {
            return None;
        }
        let meta = TokenMeta {
            mint,
            phase: TokenPhase::BondingCurve,
            trust_tier: TrustTier::BondingCurve,
            discovered_at: ts,
            source: source.to_string(),
        };
        self.tokens.insert(mint, meta.clone());
        self.curve_to_mint.insert(curve_pda, mint);
        Some(DiscoveryEvent::TokenDiscovered(meta))
    }

    /// Marks a token as graduated. Returns `None` if the mint isn't known
    /// yet, or is already marked migrated - graduation only fires once.
    pub fn observe_graduated(&mut self, mint: Pubkey, ts: i64) -> Option<DiscoveryEvent> {
        let meta = self.tokens.get_mut(&mint)?;
        if meta.phase == TokenPhase::Migrated {
            return None;
        }
        meta.phase = TokenPhase::Migrated;
        meta.trust_tier = TrustTier::MigratedNew;
        Some(DiscoveryEvent::TokenGraduated { mint, ts })
    }

    /// A graduation account update only carries the bonding-curve PDA's own
    /// pubkey; resolves it to a mint (via the reverse index) and applies
    /// the graduation in one step. Returns `None` if the curve is unknown
    /// (e.g. we connected after this token had already been created) or
    /// already graduated.
    pub fn observe_curve_completed(&mut self, curve_pda: Pubkey, ts: i64) -> Option<DiscoveryEvent> {
        let mint = self.resolve_curve(&curve_pda)?;
        self.observe_graduated(mint, ts)
    }

    /// Registers a token discovered via a path other than pump.fun (e.g. a
    /// manually configured watchlist entry) directly in the `Migrated` phase.
    pub fn observe_migrated_token(&mut self, mint: Pubkey, ts: i64, source: &str) -> Option<DiscoveryEvent> {
        if self.tokens.contains_key(&mint) {
            return None;
        }
        let meta = TokenMeta {
            mint,
            phase: TokenPhase::Migrated,
            trust_tier: TrustTier::MigratedNew,
            discovered_at: ts,
            source: source.to_string(),
        };
        self.tokens.insert(mint, meta.clone());
        Some(DiscoveryEvent::TokenDiscovered(meta))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve_for(mint: Pubkey) -> Pubkey {
        // Any distinct pubkey stands in for a bonding-curve PDA in these
        // pure-registry tests - the real PDA derivation is tested in
        // bonding_curve.rs.
        let _ = mint;
        Pubkey::new_unique()
    }

    #[test]
    fn observing_a_new_mint_registers_it_as_bonding_curve() {
        let mut reg = MintRegistry::new();
        let mint = Pubkey::new_unique();
        let curve = curve_for(mint);
        let event = reg.observe_created(mint, curve, 100, "pumpfun_create");
        assert!(matches!(event, Some(DiscoveryEvent::TokenDiscovered(_))));
        assert_eq!(reg.get(&mint).unwrap().phase, TokenPhase::BondingCurve);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn duplicate_create_sighting_is_a_no_op() {
        let mut reg = MintRegistry::new();
        let mint = Pubkey::new_unique();
        let curve = curve_for(mint);
        reg.observe_created(mint, curve, 100, "pumpfun_create");
        let second = reg.observe_created(mint, curve, 200, "pumpfun_create");
        assert!(second.is_none());
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get(&mint).unwrap().discovered_at, 100, "first sighting wins");
    }

    #[test]
    fn curve_completion_resolves_to_the_right_mint_and_graduates_it() {
        let mut reg = MintRegistry::new();
        let mint = Pubkey::new_unique();
        let curve = curve_for(mint);
        reg.observe_created(mint, curve, 100, "pumpfun_create");

        let event = reg.observe_curve_completed(curve, 500);
        assert_eq!(event, Some(DiscoveryEvent::TokenGraduated { mint, ts: 500 }));
        let meta = reg.get(&mint).unwrap();
        assert_eq!(meta.phase, TokenPhase::Migrated);
        assert_eq!(meta.trust_tier, TrustTier::MigratedNew);
    }

    #[test]
    fn graduation_only_fires_once() {
        let mut reg = MintRegistry::new();
        let mint = Pubkey::new_unique();
        let curve = curve_for(mint);
        reg.observe_created(mint, curve, 100, "pumpfun_create");
        reg.observe_curve_completed(curve, 500);
        let second = reg.observe_curve_completed(curve, 600);
        assert!(second.is_none());
    }

    #[test]
    fn completion_of_unknown_curve_is_a_no_op() {
        let mut reg = MintRegistry::new();
        let unknown_curve = Pubkey::new_unique();
        assert!(reg.observe_curve_completed(unknown_curve, 100).is_none());
    }
}
