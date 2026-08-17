//! `discovery`: automated, real-time new-token discovery. Watches pump.fun
//! bonding-curve `create` instructions (new launches), every bonding-curve
//! balance change (a live pre-graduation price feed), and `complete` flips
//! (graduations) via Yellowstone gRPC, maintaining a `MintRegistry` mapping
//! every discovered mint to a `TokenMeta`.
//!
//! Generic Raydium/Orca-native new-pool detection (for tokens that skip
//! pump.fun entirely) is a documented gap in this build, not a silent one -
//! see the README. Any pump.fun-path token, which is the overwhelming
//! majority of new Solana memecoins, is fully covered.
//!
//! Nothing this crate discovers is tradable on its own: every
//! `DiscoveryEvent::TokenDiscovered` still has to clear `safety` before
//! `risk`/`execution` ever see it - that gate lives in `bin/trading-bot`'s
//! wiring, not here.

pub mod bonding_curve;
pub mod error;
pub mod pumpfun_watcher;
pub mod registry;

pub use bonding_curve::PUMPFUN_PROGRAM_ID;
pub use error::DiscoveryError;
pub use pumpfun_watcher::{GrpcConfig, WatcherEvent};
pub use registry::{DiscoveryEvent, MintRegistry};

/// What applying a `WatcherEvent` against the registry produces: either a
/// registry-significant event (a new token, or a graduation) or a live
/// price tick for a bonding-curve token the registry already knows about.
#[derive(Debug, Clone, PartialEq)]
pub enum DiscoveryOutput {
    Event(DiscoveryEvent),
    Price(bot_core::PriceTick),
}

/// Owns the `MintRegistry` and turns raw `WatcherEvent`s from the gRPC
/// stream into resolved `DiscoveryOutput`s. Kept as a thin, synchronous,
/// fully unit-testable layer between the async watcher task and whatever
/// publishes onto the shared broadcast buses in `bin/trading-bot`.
#[derive(Default)]
pub struct Discovery {
    registry: MintRegistry,
}

impl Discovery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn registry(&self) -> &MintRegistry {
        &self.registry
    }

    pub fn apply_watcher_event(&mut self, event: WatcherEvent) -> Option<DiscoveryOutput> {
        match event {
            WatcherEvent::Created { mint, curve_pda, ts } => self
                .registry
                .observe_created(mint, curve_pda, ts, "pumpfun_create")
                .map(DiscoveryOutput::Event),
            WatcherEvent::CurveCompleted { curve_pda, ts } => {
                self.registry.observe_curve_completed(curve_pda, ts).map(DiscoveryOutput::Event)
            }
            // Only resolvable (and only emitted) for a curve whose `create`
            // we've already seen - an unresolved curve is silently dropped
            // rather than guessed at.
            WatcherEvent::CurvePriceUpdate { curve_pda, price_sol_per_token, ts } => {
                let mint = self.registry.resolve_curve(&curve_pda)?;
                Some(DiscoveryOutput::Price(bot_core::PriceTick { mint, price: price_sol_per_token, ts }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::{Pubkey, TokenPhase};

    #[test]
    fn created_then_completed_flows_through_to_a_graduated_event() {
        let mut discovery = Discovery::new();
        let mint = Pubkey::new_unique();
        let curve_pda = Pubkey::new_unique();

        let first = discovery.apply_watcher_event(WatcherEvent::Created { mint, curve_pda, ts: 100 });
        assert!(matches!(first, Some(DiscoveryOutput::Event(DiscoveryEvent::TokenDiscovered(_)))));
        assert_eq!(discovery.registry().get(&mint).unwrap().phase, TokenPhase::BondingCurve);

        let second = discovery.apply_watcher_event(WatcherEvent::CurveCompleted { curve_pda, ts: 500 });
        assert_eq!(second, Some(DiscoveryOutput::Event(DiscoveryEvent::TokenGraduated { mint, ts: 500 })));
        assert_eq!(discovery.registry().get(&mint).unwrap().phase, TokenPhase::Migrated);
    }

    #[test]
    fn completion_before_creation_is_seen_produces_no_event() {
        let mut discovery = Discovery::new();
        let curve_pda = Pubkey::new_unique();
        let event = discovery.apply_watcher_event(WatcherEvent::CurveCompleted { curve_pda, ts: 100 });
        assert!(event.is_none());
    }

    #[test]
    fn price_update_resolves_to_a_price_tick_once_the_mint_is_known() {
        let mut discovery = Discovery::new();
        let mint = Pubkey::new_unique();
        let curve_pda = Pubkey::new_unique();
        discovery.apply_watcher_event(WatcherEvent::Created { mint, curve_pda, ts: 100 });

        let output = discovery.apply_watcher_event(WatcherEvent::CurvePriceUpdate {
            curve_pda,
            price_sol_per_token: 0.000042,
            ts: 200,
        });
        match output {
            Some(DiscoveryOutput::Price(tick)) => {
                assert_eq!(tick.mint, mint);
                assert!((tick.price - 0.000042).abs() < 1e-12);
                assert_eq!(tick.ts, 200);
            }
            other => panic!("expected a Price output, got {other:?}"),
        }
    }

    #[test]
    fn price_update_for_an_unknown_curve_is_dropped_not_guessed() {
        let mut discovery = Discovery::new();
        let unknown_curve = Pubkey::new_unique();
        let output = discovery.apply_watcher_event(WatcherEvent::CurvePriceUpdate {
            curve_pda: unknown_curve,
            price_sol_per_token: 1.0,
            ts: 100,
        });
        assert!(output.is_none());
    }
}
