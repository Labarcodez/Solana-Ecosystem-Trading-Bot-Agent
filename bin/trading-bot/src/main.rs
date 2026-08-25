//! `trading-bot`: wires market_data -> strategy -> risk -> execution ->
//! storage (-> tui) into one running process, trading a static,
//! config-driven set of Kraken pairs. See README for the full
//! architecture; this file is the glue, not where the interesting logic
//! lives - that's in the library crates it calls into.

mod cli;
mod config;
mod live_feed;
mod telegram;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use bot_core::{AppEvent, LogLevel, MarketType, Pair, PairMeta, RiskTier};
use clap::Parser;
use cli::{Cli, Command, CredentialsAction, RunArgs};
use config::AppConfig;
use credentials::KrakenCredentials;
use risk::{RiskDecision, RiskManager};
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
        Command::Credentials { action } => run_credentials_command(action),
        Command::Run(args) => run(args).await,
    }
}

fn run_credentials_command(action: CredentialsAction) -> anyhow::Result<()> {
    match action {
        CredentialsAction::Init { path, futures } => {
            let path = path.unwrap_or_else(|| default_credentials_path(futures));
            let label = if futures { "Futures" } else { "Spot/Margin" };
            println!("Creating a new encrypted Kraken {label} credentials file at {}", path.display());
            credentials::init_interactive(&path).context("credentials init failed")?;
            println!("Credentials saved.");
            println!("  File: {}", path.display());
            println!();
            println!(
                "IMPORTANT: create this API key on kraken.com with ONLY \"Query Funds\", \"Query Open & \
                 Closed Orders\", and \"Create & Modify Orders\" permissions. Never grant \"Withdraw \
                 Funds\" to a key this bot holds - see README for why."
            );
            Ok(())
        }
    }
}

fn default_credentials_path(futures: bool) -> PathBuf {
    let env_var = if futures { "KRAKEN_FUTURES_CREDENTIALS_PATH" } else { "KRAKEN_CREDENTIALS_PATH" };
    let default = if futures { "./kraken_futures.enc.json" } else { "./kraken.enc.json" };
    PathBuf::from(std::env::var(env_var).unwrap_or_else(|_| default.to_string()))
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
        tracing::info!("DRY-RUN mode: real Kraken ticker calls are made, nothing is ever signed or submitted");
    } else {
        tracing::warn!("LIVE mode: real trades will be submitted using real funds");
    }

    let shutdown = CancellationToken::new();
    let storage = storage::StorageHandle::spawn(&args.db_path).context("failed to open storage")?;

    // --- Credentials (only unlocked for live mode) ---
    let spot_creds: Option<KrakenCredentials> = if !dry_run {
        let path = default_credentials_path(false);
        Some(credentials::unlock_interactive(&path).context("failed to unlock Kraken Spot/Margin credentials")?)
    } else {
        None
    };
    let futures_creds: Option<KrakenCredentials> = if !dry_run && !cfg.kraken.futures_pairs.is_empty() {
        let path = default_credentials_path(true);
        Some(credentials::unlock_interactive(&path).context("failed to unlock Kraken Futures credentials")?)
    } else {
        None
    };

    // --- Starting capital: real Kraken balance in live mode, configured
    // paper capital otherwise. ---
    let starting_capital_quote = match (&spot_creds, dry_run) {
        (Some(creds), false) => {
            fetch_live_balance_quote(&creds.api_key, &creds.api_secret, &cfg.general.base_currency)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("failed to fetch live Kraken balance ({e}), falling back to configured starting capital");
                    args.starting_capital_quote
                })
        }
        _ => args.starting_capital_quote,
    };
    tracing::info!(starting_capital_quote, "capital baseline for this session");

    // --- Shared buses ---
    let (price_tx, _) = broadcast::channel::<bot_core::PriceTick>(4096);
    let (event_tx, _) = broadcast::channel::<AppEvent>(4096);
    let (sig_tx, sig_rx) = mpsc::unbounded_channel::<bot_core::Signal>();
    // `watch`, not `broadcast`: the TUI only ever wants the *current*
    // equity/PnL snapshot, never a backlog - a new subscriber shouldn't
    // have to replay history to catch up, unlike the event log.
    let (snapshot_tx, snapshot_rx) = watch::channel(bot_core::Snapshot::default());

    // --- Static tradable set. No discovery/safety pipeline in this build
    // (see README): every pair a signal can ever be evaluated for is
    // listed in [kraken] up front, with a fixed MarketType/leverage. ---
    let mut pair_metas: HashMap<Pair, PairMeta> = HashMap::new();
    for symbol in &cfg.kraken.pairs {
        let pair = Pair::from(symbol.clone());
        pair_metas.insert(
            pair.clone(),
            PairMeta { pair, market_type: cfg.kraken.market_type, risk_tier: RiskTier::from(cfg.kraken.market_type), leverage: cfg.kraken.leverage },
        );
    }
    for symbol in &cfg.kraken.futures_pairs {
        let pair = Pair::from(symbol.clone());
        pair_metas.insert(
            pair.clone(),
            PairMeta { pair, market_type: MarketType::Futures, risk_tier: RiskTier::Futures, leverage: cfg.kraken.leverage },
        );
    }
    // Mock mode's demo asset needs an entry too, whether or not the user
    // configured any live pairs - real historical XBT/USD data either way.
    let sim_pair = Pair::from(cfg.kraken.pairs.first().cloned().unwrap_or_else(|| "XBT/USD".to_string()));
    pair_metas
        .entry(sim_pair.clone())
        .or_insert_with(|| PairMeta { pair: sim_pair.clone(), market_type: MarketType::Spot, risk_tier: RiskTier::Spot, leverage: 1.0 });
    let pair_metas = Arc::new(pair_metas);

    // --- Market data source ---
    let market_data_task = match args.price_source.as_str() {
        "mock" => {
            let path = args.mock_data.clone();
            let tx = price_tx.clone();
            let sd = shutdown.clone();
            let interval = Duration::from_millis(args.mock_tick_interval_ms);
            let sim_pair = sim_pair.clone();
            tracing::info!(data_file = %path.display(), pair = %sim_pair, "replaying mock price data (zero external API keys)");
            Some(tokio::spawn(async move {
                match market_data::mock::replay_csv(&path, sim_pair, tx, interval, sd).await {
                    Ok(n) => tracing::info!(ticks = n, "mock price replay finished"),
                    Err(e) => tracing::error!("mock price replay failed: {e}"),
                }
            }))
        }
        "live" => {
            if cfg.kraken.pairs.is_empty() && cfg.kraken.futures_pairs.is_empty() {
                tracing::warn!(
                    "--price-source live requires at least one pair in [kraken].pairs or [kraken].futures_pairs; \
                     nothing will ever become tradable this run"
                );
                None
            } else {
                tracing::info!(
                    pairs = ?cfg.kraken.pairs, futures_pairs = ?cfg.kraken.futures_pairs,
                    "starting live Kraken price feed (public, keyless endpoints)"
                );
                let handles = live_feed::spawn(
                    live_feed::LiveFeedConfig {
                        ws_url: market_data::DEFAULT_WS_URL.to_string(),
                        spot_pairs: cfg.kraken.pairs.clone(),
                        futures_pairs: cfg.kraken.futures_pairs.clone(),
                        futures_poll_secs: cfg.kraken.futures_poll_secs,
                    },
                    price_tx.clone(),
                    shutdown.clone(),
                );
                // Represented as one task for the shutdown-select below,
                // same shape as the mock replay task.
                Some(tokio::spawn(async move {
                    for handle in handles {
                        let _ = handle.await;
                    }
                }))
            }
        }
        other => anyhow::bail!("unknown --price-source {other:?} (expected \"mock\" or \"live\")"),
    };

    // --- Strategy task: price ticks in, signals out ---
    let mut strategy = strategies::build_strategy(
        &cfg.general.strategy,
        &cfg.strategy.momentum,
        &cfg.strategy.grid,
        &cfg.strategy.market_maker,
        &cfg.strategy.triangular_arbitrage,
        &cfg.strategy.funding_carry,
    )
    .with_context(|| format!("unknown strategy {:?}", cfg.general.strategy))?;
    let strategy_task = {
        let mut price_rx = price_tx.subscribe();
        let sig_tx = sig_tx.clone();
        let sd = shutdown.clone();
        tokio::spawn(async move {
            loop {
                // `biased` (checked top-to-bottom, no random tie-break) so
                // a still-buffered tick always wins over `sd.cancelled()`
                // when both happen to be ready on the same poll - without
                // it, tokio::select!'s default random choice between two
                // simultaneously-ready branches means shutdown can win the
                // coin flip and the loop exits with ticks still sitting
                // unprocessed in the channel, silently dropping whatever
                // signal they would have produced. Cancellation only wins
                // once there's genuinely nothing left to drain.
                tokio::select! {
                    biased;
                    tick = price_rx.recv() => match tick {
                        Ok(tick) => {
                            for signal in strategy.on_price_tick(&tick) {
                                let _ = sig_tx.send(signal);
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    _ = sd.cancelled() => break,
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
        let mut risk_mgr = RiskManager::new(risk_cfg, tiers, cfg.execution.slippage_bps, starting_capital_quote);
        let storage = storage.clone();
        let snapshot_tx = snapshot_tx;
        let pair_metas = pair_metas.clone();
        let spot_creds = spot_creds;
        let futures_creds = futures_creds;

        tokio::spawn(async move {
            let ctx = OrderContext {
                executor: &executor,
                storage: &storage,
                event_tx: &event_tx,
                spot_credentials: spot_creds.as_ref().map(|c| (c.api_key.as_str(), c.api_secret.as_str())),
                futures_credentials: futures_creds.as_ref().map(|c| (c.api_key.as_str(), c.api_secret.as_str())),
            };
            let mut last_price: HashMap<Pair, f64> = HashMap::new();
            // Once price_rx closes for good, its `.recv()` future resolves
            // to `Err(Closed)` immediately on every poll - under `biased`
            // ordering that would make it win every single iteration
            // forever, busy-spinning instead of ever reaching sig_rx or
            // cancellation. This guard removes that branch from the race
            // entirely once it's known-closed, same idea as the original
            // `Err(Closed) => {}` (keep looping to drain sig_rx) but
            // without pinning the CPU to do it.
            let mut price_closed = false;
            loop {
                // `biased` for the same reason as `strategy_task`'s loop:
                // without it, `sd.cancelled()` can win tokio::select!'s
                // random tie-break against a `price_rx`/`sig_rx` branch
                // that's *also* ready (buffered ticks/signals still
                // waiting), silently dropping them - including, worst
                // case, an approved buy/sell order that never gets
                // executed because the loop exited one poll too early.
                // Checking cancellation last means it only wins once both
                // channels are genuinely drained.
                tokio::select! {
                    biased;
                    tick = price_rx.recv(), if !price_closed => match tick {
                        Ok(tick) => {
                            last_price.insert(tick.pair.clone(), tick.price);
                            for order in risk_mgr.on_price_tick(&tick) {
                                handle_order(&ctx, &mut risk_mgr, order).await;
                            }
                            for ev in risk_mgr.drain_events() {
                                let _ = event_tx.send(ev);
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => { price_closed = true; }
                    },
                    signal = sig_rx.recv() => match signal {
                        Some(signal) => {
                            let mark_price = last_price.get(&signal.pair).copied().unwrap_or(0.0);
                            match pair_metas.get(&signal.pair) {
                                Some(pair_meta) => {
                                    let decision = risk_mgr.evaluate_signal(&signal, pair_meta, mark_price);
                                    match decision {
                                        RiskDecision::Approved(order) => {
                                            handle_order(&ctx, &mut risk_mgr, order).await;
                                        }
                                        RiskDecision::Rejected(reason) => {
                                            tracing::debug!(pair = %signal.pair, reason, "signal rejected by risk manager");
                                            let _ = event_tx.send(AppEvent::OrderRejected { pair: signal.pair.clone(), reason });
                                        }
                                    }
                                }
                                None => {
                                    tracing::debug!(pair = %signal.pair, "signal for a pair with no configured PairMeta; dropping");
                                }
                            }
                        }
                        None => break,
                    },
                    _ = sd.cancelled() => break,
                }
                let _ = snapshot_tx.send(bot_core::Snapshot {
                    equity_quote: risk_mgr.equity_quote(),
                    realized_pnl_quote: risk_mgr.realized_pnl_quote(),
                    unrealized_pnl_quote: risk_mgr.total_unrealized_pnl_quote(),
                    daily_pnl_quote: risk_mgr.daily_pnl_quote(),
                    daily_funding_quote: risk_mgr.daily_funding_quote(),
                    open_positions: risk_mgr.open_position_count(),
                    circuit_breaker_tripped: risk_mgr.is_breaker_tripped(),
                });
            }
        })
    };

    // --- Event logger: everything on the bus gets logged + persisted ---
    // Deliberately does NOT select on the shutdown token: a Fill produced
    // right as shutdown begins (e.g. still waiting on a slow Kraken call)
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

    // --- Optional Telegram alerts (fills, circuit-breaker trips, errors) ---
    // Same "drain before closing" reasoning as log_task: only stops once
    // every event_tx sender is gone. Never starts at all unless both
    // TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID are set in .env.
    let telegram_task = telegram::TelegramClient::from_env().map(|client| {
        let mut event_rx = event_tx.subscribe();
        tracing::info!("Telegram alerts enabled");
        tokio::spawn(async move {
            loop {
                match event_rx.recv().await {
                    Ok(event) => {
                        if let Some(text) = telegram::format_event(&event) {
                            if let Err(e) = client.send_message(&text).await {
                                tracing::warn!("failed to send Telegram alert: {e}");
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    });

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
    // Kraken call in flight) before touching the event bus, so nothing
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
    if let Some(handle) = telegram_task {
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }
    storage.shutdown();

    tracing::info!("shutdown complete");
    Ok(())
}

/// Best-effort lookup of `base_currency`'s balance out of Kraken's
/// `Balance` response. Kraken's classic asset codes commonly prefix fiat
/// with `Z` and crypto with `X` (e.g. `USD` -> `ZUSD`, `XBT` -> `XXBT`) -
/// this tries the bare code and both prefixed forms, but it is a real,
/// documented heuristic, not a full asset-code table (see README).
async fn fetch_live_balance_quote(api_key: &str, api_secret: &str, base_currency: &str) -> anyhow::Result<f64> {
    let client = execution::KrakenSpotClient::new();
    let balances = client.balance(api_key, api_secret).await.context("Balance call failed")?;
    let candidates = [base_currency.to_string(), format!("Z{base_currency}"), format!("X{base_currency}")];
    for key in &candidates {
        if let Some(raw) = balances.get(key) {
            if let Ok(value) = raw.parse::<f64>() {
                return Ok(value);
            }
        }
    }
    anyhow::bail!("no balance entry found for {base_currency} in Kraken's Balance response (tried {candidates:?})")
}

/// Bundles the pieces `handle_order` needs beyond the order itself - the
/// task-local dependencies that don't change between calls within one
/// risk/execution task. `spot_credentials`/`futures_credentials` are only
/// `Some` in live mode.
#[derive(Clone, Copy)]
struct OrderContext<'a> {
    executor: &'a execution::Executor,
    storage: &'a storage::StorageHandle,
    event_tx: &'a broadcast::Sender<AppEvent>,
    spot_credentials: Option<(&'a str, &'a str)>,
    futures_credentials: Option<(&'a str, &'a str)>,
}

async fn handle_order(ctx: &OrderContext<'_>, risk_mgr: &mut RiskManager, order: bot_core::ApprovedOrder) {
    let OrderContext { executor, storage, event_tx, spot_credentials, futures_credentials } = *ctx;
    match executor.execute(&order, spot_credentials, futures_credentials).await {
        Ok(fill) => {
            tracing::info!(
                pair = %fill.pair, side = ?fill.side, qty = fill.qty, price = fill.price,
                dry_run = fill.dry_run, "fill"
            );
            risk_mgr.on_fill(&fill);
            if let Err(e) = storage.insert_trade(&fill).await {
                tracing::error!("failed to persist trade: {e}");
            }
            let _ = event_tx.send(AppEvent::Fill(fill));
        }
        Err(e) => {
            tracing::error!(pair = %order.pair, side = ?order.side, "order execution failed: {e}");
            let _ = event_tx.send(AppEvent::OrderRejected { pair: order.pair.clone(), reason: e.to_string() });
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
                "{:?} {:.6} {} @ {:.8}{}",
                fill.side, fill.qty, fill.pair, fill.price, if fill.dry_run { " [DRY-RUN]" } else { "" }
            ),
        ),
        AppEvent::OrderRejected { pair, reason } => (LogLevel::Warn, "order_rejected", format!("{pair}: {reason}")),
        AppEvent::CircuitBreakerTripped { reason, .. } => {
            (LogLevel::Error, "circuit_breaker", format!("TRIPPED: {reason}"))
        }
        AppEvent::CircuitBreakerReset { .. } => (LogLevel::Info, "circuit_breaker", "reset".to_string()),
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
