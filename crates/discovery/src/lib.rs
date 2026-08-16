//! `discovery`: automated, real-time new-token discovery. Watches pump.fun
//! bonding-curve `create` instructions (new launches) and bonding-curve
//! `complete` flips (graduations) via Yellowstone gRPC, and maintains a
//! `MintRegistry` mapping every discovered mint to a `TokenMeta`.
//!
//! Generic Raydium/Orca-native new-pool detection (for tokens that skip
//! pump.fun entirely) is a documented gap in this build, not a silent one -
//! see the README. Any pump.fun-path token, which is the overwhelming
//! majority of new Solana memecoins, is fully covered.
//!
//! Nothing this crate discovers is tradable on its own: every
//! `DiscoveryEvent::TokenDiscovered` still has to clear `safety` before
//! `market_data`/`risk`/`execution` ever see it - that gate lives in
//! `bin/trading-bot`'s wiring, not here.

pub mod bonding_curve;
pub mod error;
pub mod pumpfun_watcher;
pub mod registry;

pub use bonding_curve::PUMPFUN_PROGRAM_ID;
pub use error::DiscoveryError;
pub use pumpfun_watcher::{GrpcConfig, WatcherEvent};
pub use registry::{DiscoveryEvent, MintRegistry};

/// Owns the `MintRegistry` and turns raw `WatcherEvent`s from the gRPC
/// stream into resolved `DiscoveryEvent`s. Kept as a thin, synchronous,
/// fully unit-testable layer between the async watcher task and whatever
/// publishes events onto the shared broadcast bus in `bin/trading-bot`.
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

    pub fn apply_watcher_event(&mut self, event: WatcherEvent) -> Option<DiscoveryEvent> {
        match event {
            WatcherEvent::Created { mint, curve_pda, ts } => {
                self.registry.observe_created(mint, curve_pda, ts, "pumpfun_create")
            }
            WatcherEvent::CurveCompleted { curve_pda, ts } => self.registry.observe_curve_completed(curve_pda, ts),
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
        assert!(matches!(first, Some(DiscoveryEvent::TokenDiscovered(_))));
        assert_eq!(discovery.registry().get(&mint).unwrap().phase, TokenPhase::BondingCurve);

        let second = discovery.apply_watcher_event(WatcherEvent::CurveCompleted { curve_pda, ts: 500 });
        assert_eq!(second, Some(DiscoveryEvent::TokenGraduated { mint, ts: 500 }));
        assert_eq!(discovery.registry().get(&mint).unwrap().phase, TokenPhase::Migrated);
    }

    #[test]
    fn completion_before_creation_is_seen_produces_no_event() {
        let mut discovery = Discovery::new();
        let curve_pda = Pubkey::new_unique();
        let event = discovery.apply_watcher_event(WatcherEvent::CurveCompleted { curve_pda, ts: 100 });
        assert!(event.is_none());
    }
}
