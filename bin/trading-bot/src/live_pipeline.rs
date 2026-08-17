//! Live token discovery, wired end-to-end: pump.fun Yellowstone watcher ->
//! `discovery::Discovery` (registry + graduation tracking) -> `safety`
//! (mandatory gate) -> a shared, dynamically-growing `TradableTokens` map
//! that the risk/execution task in `main.rs` reads from exactly the way it
//! reads the single hardcoded mock-mode asset today.
//!
//! **What this closes**: `--price-source live` used to just log a warning
//! and do nothing. Now a brand-new pump.fun token's `create` is picked up
//! live, safety-scored against real on-chain state, and - if it
//! passes - starts flowing price ticks into the same `strategy`/`risk`
//! pipeline the mock CSV replay drives, with real order execution able to
//! follow.
//!
//! **What's still a documented gap** (see README): PumpSwap/Raydium pool
//! discovery for already-migrated tokens isn't wired here, so a token's
//! live price feed stops the moment it graduates - `TokenGraduated` still
//! updates its `TrustTier`/`TokenPhase` in the tradable set (so a *manually
//! configured* migrated token, or a future PumpSwap-discovery pass, could
//! reuse the same map unchanged), but nothing currently re-establishes a
//! price feed for it. This is logged loudly, not silently, when it happens.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use bot_core::{AppEvent, LogLevel, Pubkey, TokenMeta, TokenPhase, TrustTier};
use solana_client::nonblocking::rpc_client::RpcClient;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio_util::sync::CancellationToken;

/// The bonding-curve reading needed to build a live `BondingCurveContext`,
/// cached from the same account update that produced the mint's last price
/// tick - avoids a second RPC round trip per trade attempt.
#[derive(Debug, Clone, Copy)]
pub struct CachedCurveState {
    pub virtual_token_reserves: u64,
    pub virtual_sol_reserves: u64,
    pub creator: Pubkey,
}

/// Everything the risk/execution task needs about a token that has cleared
/// discovery (and, outside mock mode, safety) - keyed by mint in
/// [`TradableTokens`].
#[derive(Debug, Clone)]
pub struct LiveTokenState {
    pub meta: TokenMeta,
    pub decimals: u8,
    /// `None` until the first bonding-curve account update arrives (or
    /// permanently, for a migrated-phase token) - a bonding-curve buy/sell
    /// attempted before this is populated is cleanly refused by the
    /// executor rather than guessed at.
    pub curve: Option<CachedCurveState>,
    /// Which SPL token program actually owns this mint (classic Token or
    /// Token-2022) - resolved once, from the mint account's own `owner`
    /// field, when the token clears safety.
    pub token_program: Option<Pubkey>,
}

pub type TradableTokens = Arc<RwLock<HashMap<Pubkey, LiveTokenState>>>;

/// Resolved lazily and cached: pump.fun's `Global.fee_recipient` is needed
/// for every bonding-curve trade but isn't something `execution::pumpfun`
/// can hardcode (see that module's docs on why). `None` until the first
/// successful fetch. Documented limitation: fetched once and never
/// refreshed after that - if pump.fun rotates it mid-run, live
/// bonding-curve trades fail cleanly (not silently) until a restart.
pub type FeeRecipientCache = Arc<Mutex<Option<Pubkey>>>;

pub struct LivePipelineConfig {
    pub grpc_endpoint: String,
    pub grpc_x_token: String,
    pub rpc_url: String,
    pub safety_cfg: safety::SafetyConfig,
}

pub struct LivePipelineHandles {
    pub tasks: Vec<tokio::task::JoinHandle<()>>,
    pub fee_recipient: FeeRecipientCache,
}

/// Spawns the pump.fun Yellowstone watcher, the discovery/safety
/// coordinator, and the fee_recipient resolver. Returns immediately - none
/// of this blocks `run()`'s startup.
pub fn spawn(
    cfg: LivePipelineConfig,
    tradable: TradableTokens,
    price_tx: broadcast::Sender<bot_core::PriceTick>,
    event_tx: broadcast::Sender<AppEvent>,
    shutdown: CancellationToken,
) -> LivePipelineHandles {
    let program_id: Pubkey = discovery::PUMPFUN_PROGRAM_ID.parse().expect("valid static program id");
    let (watcher_tx, watcher_rx) = mpsc::unbounded_channel();

    let grpc_cfg = discovery::GrpcConfig { endpoint: cfg.grpc_endpoint, x_token: cfg.grpc_x_token };
    let watcher_shutdown = shutdown.clone();
    let watcher_task = tokio::spawn(async move {
        if let Err(e) = discovery::pumpfun_watcher::watch(grpc_cfg, program_id, watcher_tx, watcher_shutdown).await {
            tracing::error!("pump.fun discovery watcher stopped: {e}");
        }
    });

    let rpc = Arc::new(RpcClient::new(cfg.rpc_url));
    let fee_recipient: FeeRecipientCache = Arc::new(Mutex::new(None));

    let fee_recipient_task = tokio::spawn(fee_recipient_updater(rpc.clone(), fee_recipient.clone(), shutdown.clone()));

    let coordinator_task =
        tokio::spawn(coordinator(rpc, cfg.safety_cfg, watcher_rx, tradable, price_tx, event_tx, shutdown));

    LivePipelineHandles { tasks: vec![watcher_task, fee_recipient_task, coordinator_task], fee_recipient }
}

async fn fetch_fee_recipient(rpc: &RpcClient) -> anyhow::Result<Pubkey> {
    let global_pda = execution::global_config_pda();
    let account = rpc.get_account(&global_pda).await.context("fetching pump.fun Global account")?;
    let fee_recipient = execution::decode_global_fee_recipient(&account.data).context("decoding Global account")?;
    Ok(fee_recipient)
}

/// Retries until it succeeds once, then stops - see [`FeeRecipientCache`]
/// docs for why a single successful reading is treated as good for the rest
/// of the run.
async fn fee_recipient_updater(rpc: Arc<RpcClient>, cache: FeeRecipientCache, shutdown: CancellationToken) {
    loop {
        match fetch_fee_recipient(&rpc).await {
            Ok(fee_recipient) => {
                *cache.lock().expect("fee_recipient cache mutex poisoned") = Some(fee_recipient);
                tracing::info!(%fee_recipient, "resolved pump.fun fee_recipient; live bonding-curve trades are now possible");
                return;
            }
            Err(e) => {
                tracing::warn!(
                    "failed to fetch pump.fun fee_recipient ({e}); live bonding-curve buys/sells are refused \
                     until this succeeds - retrying in 30s"
                );
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                }
            }
        }
    }
}

async fn coordinator(
    rpc: Arc<RpcClient>,
    safety_cfg: safety::SafetyConfig,
    mut events_rx: mpsc::UnboundedReceiver<discovery::WatcherEvent>,
    tradable: TradableTokens,
    price_tx: broadcast::Sender<bot_core::PriceTick>,
    event_tx: broadcast::Sender<AppEvent>,
    shutdown: CancellationToken,
) {
    let mut discovery_state = discovery::Discovery::new();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            ev = events_rx.recv() => {
                let Some(watcher_event) = ev else { break };
                handle_watcher_event(watcher_event, &mut discovery_state, &tradable, &price_tx, &event_tx, &rpc, &safety_cfg).await;
            }
        }
    }
}

async fn handle_watcher_event(
    watcher_event: discovery::WatcherEvent,
    discovery_state: &mut discovery::Discovery,
    tradable: &TradableTokens,
    price_tx: &broadcast::Sender<bot_core::PriceTick>,
    event_tx: &broadcast::Sender<AppEvent>,
    rpc: &Arc<RpcClient>,
    safety_cfg: &safety::SafetyConfig,
) {
    // Cache raw curve state for any mint already known, before handing the
    // event to `Discovery` (which only resolves price/graduation).
    if let discovery::WatcherEvent::CurvePriceUpdate {
        curve_pda, virtual_token_reserves, virtual_sol_reserves, creator, ..
    } = &watcher_event
    {
        if let Some(mint) = discovery_state.registry().resolve_curve(curve_pda) {
            if let Some(state) = tradable.write().await.get_mut(&mint) {
                state.curve = Some(CachedCurveState {
                    virtual_token_reserves: *virtual_token_reserves,
                    virtual_sol_reserves: *virtual_sol_reserves,
                    creator: *creator,
                });
            }
        }
    }

    match discovery_state.apply_watcher_event(watcher_event) {
        Some(discovery::DiscoveryOutput::Event(discovery::DiscoveryEvent::TokenDiscovered(meta))) => {
            let rpc = rpc.clone();
            let safety_cfg = safety_cfg.clone();
            let tradable = tradable.clone();
            let event_tx = event_tx.clone();
            // Safety scoring is a handful of RPC round trips - spawned so
            // it never blocks the coordinator from processing the next
            // discovery/price event.
            tokio::spawn(async move {
                score_and_register(rpc, safety_cfg, meta, tradable, event_tx).await;
            });
        }
        Some(discovery::DiscoveryOutput::Event(discovery::DiscoveryEvent::TokenGraduated { mint, ts })) => {
            let mut guard = tradable.write().await;
            if let Some(state) = guard.get_mut(&mint) {
                state.meta.phase = TokenPhase::Migrated;
                state.meta.trust_tier = TrustTier::MigratedNew;
            }
            drop(guard);
            tracing::warn!(
                %mint,
                "graduated; live PumpSwap price feed for migrated tokens is not wired in this build - price ticks \
                 for this mint stop here until PumpSwap pool discovery is added (see README)"
            );
            let _ = event_tx.send(AppEvent::Log {
                level: LogLevel::Warn,
                message: format!("{mint} graduated; live PumpSwap price feed not yet wired, price ticks stop here"),
                ts,
            });
        }
        Some(discovery::DiscoveryOutput::Price(tick)) => {
            // Only forward ticks for mints that have actually cleared
            // safety - `Discovery` itself doesn't know about `safety` at
            // all, so this is the gate.
            if tradable.read().await.contains_key(&tick.mint) {
                let _ = price_tx.send(tick);
            }
        }
        None => {}
    }
}

async fn score_and_register(
    rpc: Arc<RpcClient>,
    safety_cfg: safety::SafetyConfig,
    meta: TokenMeta,
    tradable: TradableTokens,
    event_tx: broadcast::Sender<AppEvent>,
) {
    use safety::TokenSafetyScorer;

    let now_ts = chrono::Utc::now().timestamp();
    // Liquidity isn't known yet at the moment of `create` - a freshly
    // launched bonding curve starts with negligible real SOL in it. This is
    // a soft check (see `safety::RuleBasedScorer`) so it flags the token as
    // low-liquidity rather than hard-rejecting it; the hard mint/freeze
    // authority checks are what actually matter for a token this new.
    let snapshot = match safety::fetch_onchain_snapshot(&rpc, &meta, None, 0.0, now_ts).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(mint = %meta.mint, "safety snapshot fetch failed: {e}");
            let _ = event_tx.send(AppEvent::TokenRejectedBySafety {
                mint: meta.mint,
                reasons: vec![format!("failed to fetch on-chain data: {e}")],
            });
            return;
        }
    };

    let verdict = safety::RuleBasedScorer.score(&meta, &snapshot, &safety_cfg);
    if !verdict.passed {
        tracing::info!(mint = %meta.mint, reasons = ?verdict.reasons, "token rejected by safety");
        let _ = event_tx.send(AppEvent::TokenRejectedBySafety { mint: meta.mint, reasons: verdict.reasons });
        return;
    }

    // Which SPL token program owns this mint - a single extra RPC call,
    // paid only for tokens that actually clear safety.
    let token_program = match rpc.get_account(&meta.mint).await {
        Ok(account) => Some(account.owner),
        Err(e) => {
            tracing::warn!(mint = %meta.mint, "failed to resolve token_program for a safety-passed token: {e}");
            None
        }
    };

    let mut meta = meta;
    meta.trust_tier = verdict.tier_hint;
    let mint = meta.mint;
    tradable.write().await.insert(
        mint,
        LiveTokenState { meta: meta.clone(), decimals: discovery::PUMPFUN_TOKEN_DECIMALS, curve: None, token_program },
    );
    tracing::info!(mint = %mint, tier = ?meta.trust_tier, "token passed safety - now tradable");
    let _ = event_tx.send(AppEvent::TokenDiscovered(meta));
}
