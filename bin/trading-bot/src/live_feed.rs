//! Live price feed for `--price-source live`: a Kraken WebSocket v2
//! `ticker` subscription for spot/margin pairs, plus an optional periodic
//! REST poll of Kraken Futures tickers (there's no Futures WebSocket
//! client in this build - see README) for perpetual pairs, funding rate
//! included. Both are public, keyless endpoints - no discovery/safety
//! pipeline sits in front of them, since this build trades a static,
//! config-driven pair list (see `config::KrakenSection` for why that's a
//! deliberate scope decision, not a gap, versus the old Solana build's
//! new-token discovery).

use std::time::Duration;

use bot_core::{Pair, PriceTick};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub struct LiveFeedConfig {
    pub ws_url: String,
    pub spot_pairs: Vec<String>,
    pub futures_pairs: Vec<String>,
    pub futures_poll_secs: u64,
}

/// Spawns whichever feeds are actually configured (either, both, or - if
/// neither list is non-empty - none) and returns their join handles.
pub fn spawn(cfg: LiveFeedConfig, price_tx: broadcast::Sender<PriceTick>, shutdown: CancellationToken) -> Vec<JoinHandle<()>> {
    let mut handles = Vec::new();

    if !cfg.spot_pairs.is_empty() {
        let tx = price_tx.clone();
        let sd = shutdown.clone();
        let ws_url = cfg.ws_url;
        let pairs = cfg.spot_pairs;
        handles.push(tokio::spawn(async move {
            if let Err(e) = market_data::stream_prices(&ws_url, &pairs, tx, sd).await {
                tracing::error!("Kraken WebSocket price stream stopped: {e}");
            }
        }));
    }

    if !cfg.futures_pairs.is_empty() {
        let tx = price_tx.clone();
        let sd = shutdown.clone();
        let pairs = cfg.futures_pairs;
        let poll_interval = Duration::from_secs(cfg.futures_poll_secs.max(5));
        handles.push(tokio::spawn(async move {
            poll_futures_tickers(pairs, poll_interval, tx, sd).await;
        }));
    }

    handles
}

async fn poll_futures_tickers(
    pairs: Vec<String>,
    poll_interval: Duration,
    tx: broadcast::Sender<PriceTick>,
    shutdown: CancellationToken,
) {
    let client = execution::KrakenFuturesClient::new();
    loop {
        match client.tickers().await {
            Ok(tickers) => {
                let now_ts = chrono::Utc::now().timestamp();
                for ticker in tickers {
                    if !pairs.iter().any(|p| p.eq_ignore_ascii_case(&ticker.symbol)) {
                        continue;
                    }
                    let Some(last) = ticker.last else { continue };
                    let _ = tx.send(PriceTick { pair: Pair::from(ticker.symbol), price: last, funding_rate: ticker.funding_rate, ts: now_ts });
                }
            }
            Err(e) => tracing::warn!("Kraken Futures ticker poll failed: {e}"),
        }
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = tokio::time::sleep(poll_interval) => {}
        }
    }
}
