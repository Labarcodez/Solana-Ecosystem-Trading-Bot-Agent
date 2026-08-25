//! Kraken WebSocket v2 public market-data client: subscribes to the
//! `ticker` channel for a configured set of pairs and turns each update
//! into a `PriceTick`. Public/keyless, unlike the old Solana build's
//! Yellowstone gRPC client - no credentials needed to watch prices on a
//! centralized exchange.
//!
//! **Verification tier:** the message schema below (subscribe request,
//! subscription ack, `ticker` data update) matches Kraken's official API
//! docs example verbatim
//! (<https://docs.kraken.com/api/docs/websocket-v2/ticker/>) - see
//! `tests::parses_the_documented_example_ticker_update`, which uses that
//! exact example payload. The `wss://ws.kraken.com/v2` endpoint itself was
//! confirmed live-reachable from this sandbox this session (a raw TLS
//! WebSocket handshake completed successfully, HTTP 101), and Kraken's
//! public REST `Ticker` endpoint was confirmed live and returned a real
//! current price - but an actual live *streamed* update was not captured
//! and fed through this parser end-to-end in this sandbox (no long-running
//! process was kept open to do so). Documented honestly as one tier below
//! `execution::kraken_spot`'s Ticker call, which *is* exercised live in
//! `dry_run`.

use bot_core::{Pair, PriceTick};
use futures::{SinkExt, StreamExt};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use crate::error::MarketDataError;

pub const DEFAULT_WS_URL: &str = "wss://ws.kraken.com/v2";

#[derive(Debug, serde::Serialize)]
struct SubscribeParams {
    channel: &'static str,
    symbol: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct SubscribeRequest {
    method: &'static str,
    params: SubscribeParams,
}

fn subscribe_request_json(pairs: &[String]) -> Result<String, MarketDataError> {
    let req = SubscribeRequest {
        method: "subscribe",
        params: SubscribeParams { channel: "ticker", symbol: pairs.to_vec() },
    };
    Ok(serde_json::to_string(&req)?)
}

#[derive(Debug, serde::Deserialize)]
struct TickerUpdateMessage {
    channel: String,
    #[serde(default)]
    data: Vec<TickerData>,
}

#[derive(Debug, serde::Deserialize)]
struct TickerData {
    symbol: String,
    last: f64,
}

/// Parses one raw WebSocket text frame into zero or more `PriceTick`s.
/// Anything that isn't a `ticker` data update (subscription acks,
/// heartbeats, other channels) is silently ignored rather than treated as
/// an error - this feed carries several message shapes and only one of
/// them is a price we care about.
fn parse_ticker_message(text: &str, now_ts: i64) -> Vec<PriceTick> {
    let Ok(msg) = serde_json::from_str::<TickerUpdateMessage>(text) else { return Vec::new() };
    if msg.channel != "ticker" {
        return Vec::new();
    }
    msg.data.into_iter().map(|d| PriceTick { pair: Pair::from(d.symbol), price: d.last, ts: now_ts }).collect()
}

/// Connects, subscribes to `ticker` for `pairs`, and forwards resolved
/// `PriceTick`s on `tx` until `shutdown` is cancelled or the stream ends.
pub async fn stream_prices(
    ws_url: &str,
    pairs: &[String],
    tx: broadcast::Sender<PriceTick>,
    shutdown: CancellationToken,
) -> Result<(), MarketDataError> {
    let (ws_stream, _response) =
        tokio_tungstenite::connect_async(ws_url).await.map_err(|e| MarketDataError::Ws(e.to_string()))?;
    let (mut write, mut read) = ws_stream.split();

    let sub_json = subscribe_request_json(pairs)?;
    write.send(Message::Text(sub_json)).await.map_err(|e| MarketDataError::Ws(e.to_string()))?;

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            msg = read.next() => {
                let Some(msg) = msg else { break };
                let msg = msg.map_err(|e| MarketDataError::Ws(e.to_string()))?;
                if let Message::Text(text) = msg {
                    let now_ts = chrono::Utc::now().timestamp();
                    for tick in parse_ticker_message(&text, now_ts) {
                        let _ = tx.send(tick);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_request_has_the_documented_shape() {
        let json = subscribe_request_json(&["ALGO/USD".to_string()]).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["method"], "subscribe");
        assert_eq!(value["params"]["channel"], "ticker");
        assert_eq!(value["params"]["symbol"][0], "ALGO/USD");
    }

    /// Kraken's official documented example update
    /// (<https://docs.kraken.com/api/docs/websocket-v2/ticker/>), quoted
    /// verbatim.
    #[test]
    fn parses_the_documented_example_ticker_update() {
        let text = r#"{
            "channel": "ticker",
            "type": "update",
            "data": [
                {
                    "symbol": "ALGO/USD",
                    "bid": 0.10025,
                    "bid_qty": 740.0,
                    "ask": 0.10035,
                    "ask_qty": 740.0,
                    "last": 0.10035,
                    "volume": 997038.98383185,
                    "vwap": 0.10148,
                    "low": 0.09979,
                    "high": 0.10285,
                    "change": -0.00017,
                    "change_pct": -0.17,
                    "timestamp": "2023-09-25T09:04:31.742648Z"
                }
            ]
        }"#;
        let ticks = parse_ticker_message(text, 1234);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].pair, Pair::from("ALGO/USD"));
        assert!((ticks[0].price - 0.10035).abs() < 1e-12);
        assert_eq!(ticks[0].ts, 1234);
    }

    #[test]
    fn subscription_acknowledgement_produces_no_ticks() {
        let text = r#"{
            "method": "subscribe",
            "result": {"channel": "ticker", "snapshot": true, "symbol": "ALGO/USD"},
            "success": true,
            "time_in": "2023-09-25T09:04:31.742599Z",
            "time_out": "2023-09-25T09:04:31.742648Z"
        }"#;
        assert!(parse_ticker_message(text, 0).is_empty());
    }

    #[test]
    fn a_non_ticker_channel_message_is_ignored() {
        let text = r#"{"channel": "heartbeat"}"#;
        assert!(parse_ticker_message(text, 0).is_empty());
    }

    #[test]
    fn malformed_json_does_not_panic() {
        assert!(parse_ticker_message("not json at all", 0).is_empty());
    }
}
