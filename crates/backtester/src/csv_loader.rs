//! Loads a `timestamp,price` CSV (unix seconds, price in quote currency)
//! into the same `PriceTick` type the live pipeline uses - this is what
//! lets the backtester replay real history through the exact same
//! `Strategy`/`RiskManager` code as a live run.

use std::path::Path;

use bot_core::{Pair, PriceTick};

use crate::error::BacktestError;

pub fn load_price_csv(path: &Path, pair: Pair) -> Result<Vec<PriceTick>, BacktestError> {
    let mut reader = csv::Reader::from_path(path)?;
    let mut ticks = Vec::new();

    for (line_no, result) in reader.records().enumerate() {
        let record = result?;
        let ts: i64 = record
            .get(0)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| BacktestError::InvalidRow(format!("row {}: bad timestamp", line_no + 2)))?;
        let price: f64 = record
            .get(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| BacktestError::InvalidRow(format!("row {}: bad price", line_no + 2)))?;
        ticks.push(PriceTick { pair: pair.clone(), price, funding_rate: None, ts });
    }

    if ticks.is_empty() {
        return Err(BacktestError::EmptyDataset(path.display().to_string()));
    }
    // Historical data must be strictly time-ordered for the strategy/risk
    // logic (SL/TP, cooldowns, day-rollover) to behave the same way it
    // would live.
    ticks.sort_by_key(|t| t.ts);
    Ok(ticks)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal temp-file helper to avoid a `tempfile` dev-dependency.
    fn write_temp_csv(name: &str, contents: &str) -> std::path::PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("bt_csv_test_{name}_{nanos}.csv"));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn loads_and_sorts_a_csv() {
        let path = write_temp_csv("sorts", "timestamp,price\n200,10.5\n100,10.0\n300,11.0\n");
        let pair = Pair::from("XBT/USD");
        let ticks = load_price_csv(&path, pair).unwrap();
        assert_eq!(ticks.len(), 3);
        assert_eq!(ticks[0].ts, 100);
        assert_eq!(ticks[1].ts, 200);
        assert_eq!(ticks[2].ts, 300);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rejects_empty_csv() {
        let path = write_temp_csv("empty", "timestamp,price\n");
        let pair = Pair::from("XBT/USD");
        assert!(load_price_csv(&path, pair).is_err());
        std::fs::remove_file(&path).ok();
    }
}
