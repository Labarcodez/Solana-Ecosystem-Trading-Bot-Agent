//! The mandatory pre-trade gate: nothing downstream (`risk`, `execution`)
//! ever sees a token this hasn't cleared. `RuleBasedScorer` is v1 - real,
//! deterministic, and fully unit-tested below. The `TokenSafetyScorer`
//! trait is the seam for a future learned/ML scorer (same shape as
//! `Strategy`'s plug-in pattern) - documented as v2+ future work, not
//! implemented in this build.

use bot_core::{TokenMeta, TokenPhase, TrustTier};

use crate::config::{OnchainSnapshot, SafetyConfig};

#[derive(Debug, Clone)]
pub struct SafetyVerdict {
    pub passed: bool,
    pub reasons: Vec<String>,
    /// Only meaningful when `passed` is true. Never `TrustTier::Established` -
    /// that tier additionally requires local trade history, which is outside
    /// a safety scorer's knowledge; it's assigned later by whatever wires
    /// this crate into the live pipeline (see README).
    pub tier_hint: TrustTier,
}

pub trait TokenSafetyScorer: Send + Sync {
    fn score(&self, token: &TokenMeta, onchain: &OnchainSnapshot, cfg: &SafetyConfig) -> SafetyVerdict;
}

pub struct RuleBasedScorer;

impl TokenSafetyScorer for RuleBasedScorer {
    fn score(&self, token: &TokenMeta, onchain: &OnchainSnapshot, cfg: &SafetyConfig) -> SafetyVerdict {
        let mut reasons = Vec::new();

        // --- Hard checks: any failure is an outright reject. ---
        if cfg.require_mint_authority_revoked && onchain.mint_authority_present {
            reasons.push("mint authority is not revoked".to_string());
        }
        if cfg.require_freeze_authority_revoked && onchain.freeze_authority_present {
            reasons.push("freeze authority is not revoked".to_string());
        }
        // LP burn/lock only applies once a token has an LP to check -
        // bonding-curve-phase tokens don't yet.
        if token.phase == TokenPhase::Migrated
            && cfg.require_lp_burned_or_locked
            && !onchain.lp_burned_or_locked
        {
            reasons.push("LP tokens are not burned or locked".to_string());
        }

        if !reasons.is_empty() {
            return SafetyVerdict { passed: false, reasons, tier_hint: TrustTier::BondingCurve };
        }

        // --- Soft checks: failures don't reject, they just keep the
        // tier hint conservative. ---
        let mut soft_ok = true;
        if onchain.liquidity_sol < cfg.min_liquidity_sol {
            reasons.push(format!(
                "liquidity {:.3} SOL below minimum {:.3} SOL",
                onchain.liquidity_sol, cfg.min_liquidity_sol
            ));
            soft_ok = false;
        }
        if onchain.top_holder_concentration_pct > cfg.max_holder_concentration_pct {
            reasons.push(format!(
                "top-holder concentration {:.1}% exceeds maximum {:.1}%",
                onchain.top_holder_concentration_pct, cfg.max_holder_concentration_pct
            ));
            soft_ok = false;
        }
        if onchain.age_secs < cfg.min_pool_age_secs {
            reasons.push(format!(
                "age {}s below minimum {}s",
                onchain.age_secs, cfg.min_pool_age_secs
            ));
            soft_ok = false;
        }

        let tier_hint = match token.phase {
            TokenPhase::BondingCurve => TrustTier::BondingCurve,
            TokenPhase::Migrated => TrustTier::MigratedNew,
        };
        let _ = soft_ok; // soft checks currently only inform `reasons`, not rejection

        SafetyVerdict { passed: true, reasons, tier_hint }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::Pubkey;

    fn token(phase: TokenPhase) -> TokenMeta {
        TokenMeta {
            mint: Pubkey::new_unique(),
            phase,
            trust_tier: TrustTier::BondingCurve,
            discovered_at: 0,
            source: "test".into(),
        }
    }

    fn clean_snapshot() -> OnchainSnapshot {
        OnchainSnapshot {
            mint_authority_present: false,
            freeze_authority_present: false,
            lp_burned_or_locked: true,
            top_holder_concentration_pct: 10.0,
            liquidity_sol: 50.0,
            age_secs: 3600,
        }
    }

    #[test]
    fn clean_migrated_token_passes_with_migrated_new_tier() {
        let scorer = RuleBasedScorer;
        let verdict = scorer.score(&token(TokenPhase::Migrated), &clean_snapshot(), &SafetyConfig::default());
        assert!(verdict.passed);
        assert_eq!(verdict.tier_hint, TrustTier::MigratedNew);
        assert!(verdict.reasons.is_empty());
    }

    #[test]
    fn scorer_never_hints_established_tier() {
        // Established additionally requires local trade history, which a
        // safety scorer can't know - it must never be the tier_hint here,
        // clean snapshot or not.
        let scorer = RuleBasedScorer;
        let verdict = scorer.score(&token(TokenPhase::Migrated), &clean_snapshot(), &SafetyConfig::default());
        assert_ne!(verdict.tier_hint, TrustTier::Established);
    }

    #[test]
    fn unrevoked_mint_authority_is_a_hard_reject() {
        let scorer = RuleBasedScorer;
        let mut snap = clean_snapshot();
        snap.mint_authority_present = true;
        let verdict = scorer.score(&token(TokenPhase::Migrated), &snap, &SafetyConfig::default());
        assert!(!verdict.passed);
        assert!(verdict.reasons.iter().any(|r| r.contains("mint authority")));
    }

    #[test]
    fn unrevoked_freeze_authority_is_a_hard_reject() {
        let scorer = RuleBasedScorer;
        let mut snap = clean_snapshot();
        snap.freeze_authority_present = true;
        let verdict = scorer.score(&token(TokenPhase::Migrated), &snap, &SafetyConfig::default());
        assert!(!verdict.passed);
    }

    #[test]
    fn unburned_lp_is_a_hard_reject_for_migrated_tokens() {
        let scorer = RuleBasedScorer;
        let mut snap = clean_snapshot();
        snap.lp_burned_or_locked = false;
        let verdict = scorer.score(&token(TokenPhase::Migrated), &snap, &SafetyConfig::default());
        assert!(!verdict.passed);
    }

    #[test]
    fn lp_check_is_skipped_for_bonding_curve_tokens() {
        let scorer = RuleBasedScorer;
        let mut snap = clean_snapshot();
        snap.lp_burned_or_locked = false; // no LP exists yet pre-graduation
        let verdict = scorer.score(&token(TokenPhase::BondingCurve), &snap, &SafetyConfig::default());
        assert!(verdict.passed);
        assert_eq!(verdict.tier_hint, TrustTier::BondingCurve);
    }

    #[test]
    fn low_liquidity_is_a_soft_fail_not_a_reject() {
        let scorer = RuleBasedScorer;
        let mut snap = clean_snapshot();
        snap.liquidity_sol = 0.1;
        let cfg = SafetyConfig::default();
        let verdict = scorer.score(&token(TokenPhase::Migrated), &snap, &cfg);
        assert!(verdict.passed, "low liquidity should not hard-reject");
        assert!(verdict.reasons.iter().any(|r| r.contains("liquidity")));
    }

    #[test]
    fn high_holder_concentration_is_a_soft_fail() {
        let scorer = RuleBasedScorer;
        let mut snap = clean_snapshot();
        snap.top_holder_concentration_pct = 90.0;
        let verdict = scorer.score(&token(TokenPhase::Migrated), &snap, &SafetyConfig::default());
        assert!(verdict.passed);
        assert!(verdict.reasons.iter().any(|r| r.contains("concentration")));
    }

    #[test]
    fn stricter_config_thresholds_change_the_outcome() {
        // Same snapshot, different config -> different verdict. Confirms
        // the scorer actually reads its thresholds instead of hardcoding a result.
        let scorer = RuleBasedScorer;
        let snap = clean_snapshot(); // liquidity_sol = 50.0
        let loose = SafetyConfig { min_liquidity_sol: 1.0, ..SafetyConfig::default() };
        let strict = SafetyConfig { min_liquidity_sol: 1000.0, ..SafetyConfig::default() };

        let loose_verdict = scorer.score(&token(TokenPhase::Migrated), &snap, &loose);
        let strict_verdict = scorer.score(&token(TokenPhase::Migrated), &snap, &strict);
        assert!(loose_verdict.reasons.is_empty());
        assert!(!strict_verdict.reasons.is_empty());
    }
}
