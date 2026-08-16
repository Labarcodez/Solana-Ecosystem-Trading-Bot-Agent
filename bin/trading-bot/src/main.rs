//! `trading-bot`: wires discovery -> safety -> market_data -> strategy ->
//! risk -> execution -> storage (-> tui) into one running process. See
//! README for the full architecture; this file is the glue, not where the
//! interesting logic lives - that's in the library crates it calls into.

mod cli;
mod config;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use bot_core::{AppEvent, LogLevel, Pubkey, TokenMeta, TokenPhase, TrustTier};
use clap::Parser;
use cli::{Cli, Command, RunArgs, WalletAction};
use config::AppConfig;
use risk::{RiskDecision, RiskManager};
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive(
        "trading_bot=info".parse().unwrap(),
    )).init();
    dotenvy::dotenv().ok();

    let cli = Cli::parse();
    match cli.command {
        Command::Wallet { action } => run_wallet_command(action),
        Command::Run(args) => run(args).await,
    }
}

fn run_wallet_command(action: WalletAction) -> anyhow::Result<()> {
    match action {
        WalletAction::Init { path } => {
            let path = path.unwrap_or_else(default_wallet_path);
            println!("Creating a new encrypted wallet at {}", path.display());
            let pubkey = wallet::init_interactive(&path).context("wallet init failed")?;
            println!("Wallet created.");
            println!("  Public key: {pubkey}");
            println!("  Key file:   {}", path.display());
            println!("Fund this address with SOL before running in live mode.");
            Ok(())
        }
    }
}

fn default_wallet_path() -> PathBuf {
    PathBuf::from(std::env::var("WALLET_KEY_PATH").unwrap_or_else(|_| "./wallet.enc.json".to_string()))
}

async fn run(args: RunArgs) -> anyhow::Result<()> {
    let config_str = std::fs::read_to_string(&args.config)
        .with_context(|| format!("reading config file {}", args.config.display()))?;
    let cfg: AppConfig =
        toml::from_str(&config_str).with_context(|| format!("parsing config file {}", args.config.display()))?;

    let mode = args.mode.clone().unwrap_or_else(|| cfg.general.mode.clone());
    let dry_run = mode != "live";
    let (risk_cfg, tiers) = cfg.risk.clone().into_parts();
    if tiers.is_empty() {
        anyhow::bail!("no valid [risk.tiers.*] sections found in {}", args.config.display());
    }

    tracing::info!(
        mode, dry_run, strategy = %cfg.general.strategy, price_source = %args.price_source,
        "starting trading-bot"
    );
    if dry_run {
        tracing::info!("DRY-RUN mode: real quotes are fetched, nothing is ever signed or submitted on-chain");
    } else {
        tracing::warn!("LIVE mode: real trades will be submitted using real funds");
    }

    let shutdown = CancellationToken::new();
    let storage = storage::StorageHandle::spawn(&args.db_path).context("failed to open storage")?;

    // --- Wallet (only unlocked for live mode) ---
    let wallet_keypair: Option<Keypair> = if !dry_run {
        let path = default_wallet_path();
        Some(wallet::unlock_interactive(&path).context("failed to unlock wallet")?)
    } else {
        None
    };

    // --- Starting capital: real balance in live mode, configured paper
    // capital otherwise. ---
    let starting_capital_sol = match (&wallet_keypair, dry_run) {
        (Some(kp), false) => fetch_live_balance_sol(kp).await.unwrap_or_else(|e| {
            tracing::warn!("failed to fetch live SOL balance ({e}), falling back to configured starting capital");
            args.starting_capital_sol
        }),
        _ => args.starting_capital_sol,
    };
    tracing::info!(starting_capital_sol, "capital baseline for this session");

    // --- Shared buses ---
    let (price_tx, _) = broadcast::channel::<bot_core::PriceTick>(4096);
    let (event_tx, _) = broadcast::channel::<AppEvent>(4096);
    let (sig_tx, sig_rx) = mpsc::unbounded_channel::<bot_core::Signal>();
    // `watch`, not `broadcast`: the TUI only ever wants the *current*
    // equity/PnL snapshot, never a backlog - a new subscriber shouldn't
    // have to replay history to catch up, unlike the event log.
    let (snapshot_tx, snapshot_rx) = watch::channel(bot_core::Snapshot::default());

    // --- Market data source ---
    // Real USDC mint (6 decimals) - data/sample_sol_usdc.csv is a real
    // SOL/USD price series, and using an actual Jupiter-tradable mint here
    // (rather than a random Pubkey) is what lets dry-run's real Jupiter
    // /quote call succeed: the mock feed drives strategy/risk timing, the
    // quote itself always reflects real live market data for whatever mint
    // is configured, by design - these are two independent concerns.
    let sim_mint: Pubkey = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
        .parse()
        .expect("valid static USDC mint");
    const SIM_MINT_DECIMALS: u8 = 6;
    let market_data_task = match args.price_source.as_str() {
        "mock" => {
            let path = args.mock_data.clone();
            let tx = price_tx.clone();
            let sd = shutdown.clone();
            let interval = Duration::from_millis(args.mock_tick_interval_ms);
            tracing::info!(data_file = %path.display(), "replaying mock price data (zero external API keys)");
            Some(tokio::spawn(async move {
                match market_data::mock::replay_csv(&path, sim_mint, tx, interval, sd).await {
                    Ok(n) => tracing::info!(ticks = n, "mock price replay finished"),
                    Err(e) => tracing::error!("mock price replay failed: {e}"),
                }
            }))
        }
        "live" => {
            tracing::warn!(
                "--price-source live requires SHYFT_GRPC_ENDPOINT/SHYFT_X_TOKEN and a configured watchlist; \
                 discovery/market_data/safety live wiring is documented in the README as needing your own \
                 credentials and is not exercised by this run"
            );
            None
        }
        other => anyhow::bail!("unknown --price-source {other:?} (expected \"mock\" or \"live\")"),
    };

    // --- Strategy task: price ticks in, signals out ---
    let mut strategy =
        strategies::build_strategy(&cfg.general.strategy, &cfg.strategy.momentum, &cfg.strategy.grid)
            .with_context(|| format!("unknown strategy {:?}", cfg.general.strategy))?;
    let strategy_task = {
        let mut price_rx = price_tx.subscribe();
        let sig_tx = sig_tx.clone();
        let sd = shutdown.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = sd.cancelled() => break,
                    tick = price_rx.recv() => match tick {
                        Ok(tick) => {
                            for signal in strategy.on_price_tick(&tick) {
                                let _ = sig_tx.send(signal);
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        })
    };
    drop(sig_tx); // the clone above is what the strategy task keeps; drop main's copy so sig_rx closes when it exits

    // --- Risk + execution task: the single path from signal to fill ---
    let risk_execution_task = {
        let mut price_rx = price_tx.subscribe();
        let event_tx = event_tx.clone();
        let sd = shutdown.clone();
        let mut sig_rx = sig_rx;
        let executor = execution::Executor::new(dry_run);
        let mut risk_mgr = RiskManager::new(risk_cfg, tiers, cfg.execution.slippage_bps, starting_capital_sol);
        let storage = storage.clone();
        let wallet_keypair = wallet_keypair;
        let snapshot_tx = snapshot_tx;

        // In mock mode there is exactly one tradable asset; in a future
        // live wiring this comes from discovery's MintRegistry (see
        // discovery::Discovery / safety::TokenSafetyScorer) instead of a
        // single hardcoded entry.
        let sim_token_meta = TokenMeta {
            mint: sim_mint,
            phase: TokenPhase::Migrated,
            trust_tier: TrustTier::Established,
            discovered_at: 0,
            source: "mock".to_string(),
        };

        tokio::spawn(async move {
            let ctx = OrderContext {
                executor: &executor,
                storage: &storage,
                event_tx: &event_tx,
                mint_decimals: SIM_MINT_DECIMALS,
                wallet: wallet_keypair.as_ref(),
            };
            let mut last_price: HashMap<Pubkey, f64> = HashMap::new();
            loop {
                tokio::select! {
                    _ = sd.cancelled() => break,
                    tick = price_rx.recv() => match tick {
                        Ok(tick) => {
                            last_price.insert(tick.mint, tick.price);
                            for order in risk_mgr.on_price_tick(&tick) {
                                handle_order(&ctx, &mut risk_mgr, order, tick.price).await;
                            }
                            for ev in risk_mgr.drain_events() {
                                let _ = event_tx.send(ev);
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => {}
                    },
                    signal = sig_rx.recv() => match signal {
                        Some(signal) => {
                            let mark_price = last_price.get(&signal.mint).copied().unwrap_or(0.0);
                            let decision = risk_mgr.evaluate_signal(&signal, &sim_token_meta, mark_price);
                            match decision {
                                RiskDecision::Approved(order) => {
                                    handle_order(&ctx, &mut risk_mgr, order, mark_price).await;
                                }
                                RiskDecision::Rejected(reason) => {
                                    tracing::debug!(mint = %signal.mint, reason, "signal rejected by risk manager");
                                    let _ = event_tx.send(AppEvent::OrderRejected { mint: signal.mint, reason });
                                }
                            }
                        }
                        None => break,
                    }
                }
                let _ = snapshot_tx.send(bot_core::Snapshot {
                    equity_sol: risk_mgr.equity_sol(),
                    realized_pnl_sol: risk_mgr.realized_pnl_sol(),
                    unrealized_pnl_sol: risk_mgr.total_unrealized_pnl_sol(),
                    daily_pnl_sol: risk_mgr.daily_pnl_sol(),
                    open_positions: risk_mgr.open_position_count(),
                    circuit_breaker_tripped: risk_mgr.is_breaker_tripped(),
                });
            }
        })
    };

    // --- Event logger: everything on the bus gets logged + persisted ---
    // Deliberately does NOT select on the shutdown token: a Fill produced
    // right as shutdown begins (e.g. still waiting on a slow Jupiter quote)
    // must still get logged/persisted, not dropped because the logger won
    // a race against its own cancellation. It exits only once every
    // `event_tx` sender is gone (see the shutdown sequence below), which
    // guarantees everything already-sent has been drained first.
    let log_task = {
        let mut event_rx = event_tx.subscribe();
        let storage = storage.clone();
        tokio::spawn(async move {
            loop {
                match event_rx.recv().await {
                    Ok(event) => log_event(&storage, &event).await,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    };

    // --- TUI dashboard (unless --no-tui) ---
    // Read-only subscriber over the same buses everything else uses - see
    // tui::dashboard for why a slow redraw can't backpressure the trading
    // loop. `q` inside the dashboard sends ControlCommand::Quit here, which
    // drives the same shutdown path as Ctrl-C.
    let (tui_task, mut tui_quit_rx) = if !args.no_tui {
        let channels = tui::DashboardChannels {
            price_rx: price_tx.subscribe(),
            event_rx: event_tx.subscribe(),
            snapshot_rx: snapshot_rx.clone(),
        };
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        let (quit_tx, quit_rx) = mpsc::unbounded_channel::<()>();
        let sd = shutdown.clone();
        let mode_label = mode.clone();
        let strategy_label = cfg.general.strategy.clone();
        let handle = tokio::spawn(async move {
            if let Err(e) = tui::run(mode_label, strategy_label, channels, control_tx, sd).await {
                tracing::error!("tui error (is this running in a real terminal?): {e}");
            }
        });
        tokio::spawn(async move {
            while let Some(cmd) = control_rx.recv().await {
                if cmd == tui::ControlCommand::Quit {
                    let _ = quit_tx.send(());
                }
            }
        });
        (Some(handle), Some(quit_rx))
    } else {
        (None, None)
    };

    // --- Wait for shutdown: Ctrl-C, 'q' in the dashboard, or (in mock
    // mode) the replay finishing ---
    let quit_signal = async {
        match tui_quit_rx.as_mut() {
            Some(rx) => {
                rx.recv().await;
            }
            None => std::future::pending::<()>().await,
        }
    };
    match market_data_task {
        Some(handle) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => tracing::info!("received Ctrl-C, shutting down"),
                _ = quit_signal => tracing::info!("dashboard quit requested, shutting down"),
                _ = handle => tracing::info!("price source finished, shutting down"),
            }
        }
        None => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => tracing::info!("received Ctrl-C, shutting down"),
                _ = quit_signal => tracing::info!("dashboard quit requested, shutting down"),
            }
        }
    }

    shutdown.cancel();
    // Let the producer tasks finish whatever they're mid-await on (e.g. a
    // Jupiter quote in flight) before touching the event bus, so nothing
    // they send gets lost.
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        let _ = strategy_task.await;
        let _ = risk_execution_task.await;
        if let Some(handle) = tui_task {
            let _ = handle.await;
        }
    })
    .await;
    // Now that both producers are done, dropping our own sender is what
    // finally closes the broadcast channel for `log_task` - it will have
    // drained every event sent above before its `recv()` returns `Closed`.
    drop(event_tx);
    let _ = tokio::time::timeout(Duration::from_secs(5), log_task).await;
    storage.shutdown();

    tracing::info!("shutdown complete");
    Ok(())
}

async fn fetch_live_balance_sol(wallet: &Keypair) -> anyhow::Result<f64> {
    let rpc_url = std::env::var("ALCHEMY_RPC_URL").context("ALCHEMY_RPC_URL not set in .env")?;
    let rpc = solana_client::nonblocking::rpc_client::RpcClient::new(rpc_url);
    let lamports = rpc.get_balance(&wallet.pubkey()).await.context("get_balance failed")?;
    Ok(lamports as f64 / 1_000_000_000.0)
}

/// Bundles the pieces `handle_order` needs beyond the order itself and the
/// current mark price - the task-local dependencies that don't change
/// between calls within one risk/execution task.
#[derive(Clone, Copy)]
struct OrderContext<'a> {
    executor: &'a execution::Executor,
    storage: &'a storage::StorageHandle,
    event_tx: &'a broadcast::Sender<AppEvent>,
    mint_decimals: u8,
    wallet: Option<&'a Keypair>,
}

async fn handle_order(
    ctx: &OrderContext<'_>,
    risk_mgr: &mut RiskManager,
    order: bot_core::ApprovedOrder,
    mark_price: f64,
) {
    let OrderContext { executor, storage, event_tx, mint_decimals, wallet } = *ctx;
    match executor.execute(&order, mark_price, mint_decimals, wallet, None).await {
        Ok(fill) => {
            tracing::info!(
                mint = %fill.mint, side = ?fill.side, qty = fill.qty, price = fill.price,
                dry_run = fill.dry_run, "fill"
            );
            risk_mgr.on_fill(&fill);
            if let Err(e) = storage.insert_trade(&fill).await {
                tracing::error!("failed to persist trade: {e}");
            }
            let _ = event_tx.send(AppEvent::Fill(fill));
        }
        Err(e) => {
            tracing::error!(mint = %order.mint, side = ?order.side, "order execution failed: {e}");
            let _ = event_tx.send(AppEvent::OrderRejected { mint: order.mint, reason: e.to_string() });
        }
    }
}

async fn log_event(storage: &storage::StorageHandle, event: &AppEvent) {
    let ts = chrono::Utc::now().timestamp();
    let (level, kind, message) = match event {
        AppEvent::Fill(fill) => (
            LogLevel::Info,
            "fill",
            format!(
                "{:?} {:.6} {} @ {:.8} SOL{}",
                fill.side, fill.qty, fill.mint, fill.price, if fill.dry_run { " [DRY-RUN]" } else { "" }
            ),
        ),
        AppEvent::OrderRejected { mint, reason } => (LogLevel::Warn, "order_rejected", format!("{mint}: {reason}")),
        AppEvent::CircuitBreakerTripped { reason, .. } => {
            (LogLevel::Error, "circuit_breaker", format!("TRIPPED: {reason}"))
        }
        AppEvent::CircuitBreakerReset { .. } => (LogLevel::Info, "circuit_breaker", "reset".to_string()),
        AppEvent::TokenDiscovered(meta) => {
            (LogLevel::Info, "discovery", format!("discovered {} ({:?})", meta.mint, meta.phase))
        }
        AppEvent::TokenRejectedBySafety { mint, reasons } => {
            (LogLevel::Warn, "safety_rejected", format!("{mint}: {}", reasons.join("; ")))
        }
        AppEvent::Log { level, message, .. } => (*level, "log", message.clone()),
    };
    match level {
        LogLevel::Info => tracing::info!(kind, "{message}"),
        LogLevel::Warn => tracing::warn!(kind, "{message}"),
        LogLevel::Error => tracing::error!(kind, "{message}"),
    }
    if let Err(e) = storage.insert_event(ts, level, kind, &message).await {
        tracing::error!("failed to persist event: {e}");
    }
}
