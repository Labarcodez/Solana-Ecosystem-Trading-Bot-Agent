//! Replays a `timestamp,price` CSV as a live-shaped `PriceTick` stream. This
//! is what powers `--price-source mock`: the entire pipeline (strategy,
//! risk, executor in dry-run, storage, TUI) can run end-to-end against real
//! historical data with zero external API keys and zero cost.

use std::path::Path;
use std::time::Duration;

use bot_core::{Pair, PriceTick};
use tokio::sync::broadcast;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::error::MarketDataError;

pub async fn replay_csv(
    path: &Path,
    pair: Pair,
    tx: broadcast::Sender<PriceTick>,
    tick_interval: Duration,
    shutdown: CancellationToken,
) -> Result<usize, MarketDataError> {
    let mut reader = csv::Reader::from_path(path)?;
    let mut rows: Vec<(i64, f64)> = Vec::new();
    for result in reader.records() {
        let record = result?;
        let ts: i64 = record.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
        let price: f64 = record.get(1).and_then(|s| s.parse().ok()).unwrap_or(0.0);
        rows.push((ts, price));
    }
    rows.sort_by_key(|(ts, _)| *ts);

    let mut sent = 0usize;
    for (ts, price) in rows {
        if shutdown.is_cancelled() {
            break;
        }
        // A send error just means there are currently no subscribers -
        // harmless for a broadcast channel, keep replaying.
        let _ = tx.send(PriceTick { pair: pair.clone(), price, ts });
        sent += 1;

        if tick_interval.is_zero() {
            continue;
        }
        tokio::select! {
            _ = sleep(tick_interval) => {}
            _ = shutdown.cancelled() => break,
        }
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp_csv(name: &str, contents: &str) -> std::path::PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("md_mock_test_{name}_{nanos}.csv"));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[tokio::test]
    async fn replays_ticks_in_timestamp_order() {
        let path = write_temp_csv("order", "timestamp,price\n200,2.0\n100,1.0\n300,3.0\n");
        let pair = Pair::from("XBT/USD");
        let (tx, mut rx) = broadcast::channel(16);
        let shutdown = CancellationToken::new();

        let sent = replay_csv(&path, pair, tx, Duration::ZERO, shutdown).await.unwrap();
        assert_eq!(sent, 3);

        let mut ts_seen = Vec::new();
        while let Ok(tick) = rx.try_recv() {
            ts_seen.push(tick.ts);
        }
        assert_eq!(ts_seen, vec![100, 200, 300]);

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn shutdown_token_stops_replay_early() {
        let mut content = String::from("timestamp,price\n");
        for i in 0..1000 {
            content.push_str(&format!("{i},1.0\n"));
        }
        let path = write_temp_csv("shutdown", &content);
        let pair = Pair::from("XBT/USD");
        let (tx, _rx) = broadcast::channel(2000);
        let shutdown = CancellationToken::new();
        shutdown.cancel(); // cancelled before we even start

        let sent = replay_csv(&path, pair, tx, Duration::from_millis(1), shutdown).await.unwrap();
        assert_eq!(sent, 0, "already-cancelled token should stop replay immediately");

        std::fs::remove_file(&path).ok();
    }
}
