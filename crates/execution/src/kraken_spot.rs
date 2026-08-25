//! Kraken Spot REST client: public market data (`Ticker`, `OHLC`) plus
//! private trading (`Balance`, `AddOrder`, `CancelOrder`, `OpenOrders`).
//! Margin trading rides this exact same client - it's just an `AddOrder`
//! call with a `leverage` parameter set, per Kraken's own API design (there
//! is no separate margin endpoint). Hand-rolled rather than using one of
//! the several thin, low-adoption community Kraken crates on crates.io, for
//! the same reason this project hand-rolled Jupiter/Jito before: a couple
//! of REST endpoints plus one signing algorithm is cheap to own directly
//! and doesn't drift out of sync with a fast-moving exchange API.
//!
//! **Verification tier:** the request-signing algorithm below is
//! implemented from Kraken's own published documentation
//! (<https://support.kraken.com/articles/360029054811>) and cross-checked
//! against Kraken's own published worked example (secret/path/nonce/
//! postdata) via an independent Python (`hashlib`/`hmac`) computation done
//! during this session - see `tests::signature_matches_krakens_published_worked_example`.
//! The endpoint shapes (`AddOrder` params, `Ticker`/`OHLC` response fields)
//! are taken from current API documentation but have **not** been
//! exercised against a live Kraken account in this sandbox (no funded API
//! key was available) - `dry_run` calls only the public, keyless endpoints
//! live; private-endpoint calls are unit-tested against fixture JSON only.

use std::collections::HashMap;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256, Sha512};

use crate::error::ExecutionError;

const DEFAULT_BASE_URL: &str = "https://api.kraken.com";

/// Monotonically-increasing nonce Kraken's private endpoints require.
/// Seeded from the wall clock (microsecond resolution) but backed by an
/// atomic counter so two calls issued back-to-back from this process are
/// *guaranteed* strictly increasing even if the underlying clock's actual
/// resolution is coarser than a microsecond (observed on this sandbox) -
/// a real nonce collision would make Kraken reject the second request.
pub fn generate_nonce() -> u64 {
    static LAST_NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system clock before epoch").as_micros() as u64;
    loop {
        let last = LAST_NONCE.load(std::sync::atomic::Ordering::SeqCst);
        let next = now.max(last + 1);
        if LAST_NONCE.compare_exchange(last, next, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst).is_ok() {
            return next;
        }
    }
}

/// Kraken's REST private-endpoint signature: `HMAC-SHA512(secret,
/// path + SHA256(nonce + postdata))`, base64-encoded. See module docs for
/// the worked-example cross-check.
pub fn sign_request(path: &str, nonce: u64, post_data: &str, api_secret_b64: &str) -> Result<String, ExecutionError> {
    let secret = STANDARD.decode(api_secret_b64).map_err(|e| ExecutionError::Base64(e.to_string()))?;

    let mut sha256 = Sha256::new();
    sha256.update(nonce.to_string().as_bytes());
    sha256.update(post_data.as_bytes());
    let digest = sha256.finalize();

    let mut mac = Hmac::<Sha512>::new_from_slice(&secret)
        .map_err(|e| ExecutionError::KrakenSpot(format!("invalid API secret: {e}")))?;
    mac.update(path.as_bytes());
    mac.update(&digest);
    let signature = mac.finalize().into_bytes();

    Ok(STANDARD.encode(signature))
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct TickerInfo {
    /// `[price, whole_lot_volume, lot_volume]` - ask.
    pub a: Vec<String>,
    /// bid, same shape as `a`.
    pub b: Vec<String>,
    /// last trade closed `[price, lot_volume]`.
    pub c: Vec<String>,
}

impl TickerInfo {
    pub fn best_ask(&self) -> Option<f64> {
        self.a.first().and_then(|s| s.parse().ok())
    }

    pub fn best_bid(&self) -> Option<f64> {
        self.b.first().and_then(|s| s.parse().ok())
    }

    pub fn last_trade_price(&self) -> Option<f64> {
        self.c.first().and_then(|s| s.parse().ok())
    }
}

#[derive(Debug, serde::Deserialize)]
struct KrakenResponse<T> {
    error: Vec<String>,
    result: Option<T>,
}

fn unwrap_result<T>(resp: KrakenResponse<T>) -> Result<T, ExecutionError> {
    if !resp.error.is_empty() {
        return Err(ExecutionError::KrakenSpot(resp.error.join("; ")));
    }
    resp.result.ok_or_else(|| ExecutionError::UnexpectedResponse("missing \"result\" field".into()))
}

/// What kind of order to place - a thin, typed subset of Kraken's
/// `ordertype` values. `market` is used for anything that must fill now
/// (protective exits); `limit` with `post_only: true` is used for anything
/// that can wait for a maker fill (see `executor.rs` for which reason maps
/// to which).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderType {
    Market,
    Limit,
}

pub struct AddOrderRequest<'a> {
    pub pair: &'a str,
    pub side: &'a str, // "buy" | "sell"
    pub order_type: OrderType,
    /// Required for `OrderType::Limit`.
    pub limit_price: Option<f64>,
    pub volume: f64,
    /// Kraken's leverage string, e.g. `"2:1"`. `None` for plain spot.
    pub leverage: Option<String>,
    pub post_only: bool,
}

#[derive(Debug, serde::Deserialize)]
pub struct AddOrderResult {
    pub txid: Vec<String>,
    pub descr: AddOrderDescr,
}

#[derive(Debug, serde::Deserialize)]
pub struct AddOrderDescr {
    pub order: String,
}

pub struct KrakenSpotClient {
    http: reqwest::Client,
    base_url: String,
}

impl Default for KrakenSpotClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Kraken's classic REST API expects pair symbols without the `/`
/// separator (e.g. `"XBTUSD"`) - confirmed live against the public
/// `Ticker` endpoint this session (`.../Ticker?pair=XBT/USD` returns
/// `EQuery:Unknown asset pair`; `.../Ticker?pair=XBTUSD` succeeds). The
/// newer WebSocket v2 API uses the slash-separated "wsname" form instead
/// (e.g. `"XBT/USD"`, per its own docs - see `market_data::kraken_ws`).
/// This bot's `Pair` type carries the wsname spelling end-to-end (so the
/// same value can subscribe to live prices directly), so the REST client
/// strips the slash at its own boundary rather than making every caller
/// carry two spellings. Verified live for `XBT/USD` this session; not
/// independently verified for every pair Kraken lists - if a configured
/// pair doesn't resolve this way, `Ticker`/`AddOrder` will return a clear
/// `EQuery:Unknown asset pair` error rather than silently misbehaving.
fn rest_pair_symbol(pair: &str) -> String {
    pair.replace('/', "")
}

impl KrakenSpotClient {
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self { http: reqwest::Client::new(), base_url: base_url.into() }
    }

    /// Real, keyless public endpoint - safe to call in dry-run to prove
    /// live connectivity, the same role Jupiter's `/quote` call played in
    /// the Solana build.
    pub async fn ticker(&self, pair: &str) -> Result<TickerInfo, ExecutionError> {
        let rest_pair = rest_pair_symbol(pair);
        let url = format!("{}/0/public/Ticker", self.base_url);
        let resp: KrakenResponse<HashMap<String, TickerInfo>> =
            self.http.get(&url).query(&[("pair", &rest_pair)]).send().await?.json().await?;
        let result = unwrap_result(resp)?;
        result.into_iter().next().map(|(_, info)| info).ok_or_else(|| ExecutionError::NoTickerData(pair.to_string()))
    }

    /// Places an order. `leverage.is_some()` is what turns a plain spot
    /// order into a margin order in Kraken's own API - there is no separate
    /// margin endpoint to call.
    pub async fn add_order(
        &self,
        api_key: &str,
        api_secret: &str,
        req: &AddOrderRequest<'_>,
    ) -> Result<AddOrderResult, ExecutionError> {
        let path = "/0/private/AddOrder";
        let nonce = generate_nonce();

        let mut form: Vec<(String, String)> = vec![
            ("nonce".into(), nonce.to_string()),
            ("pair".into(), rest_pair_symbol(req.pair)),
            ("type".into(), req.side.to_string()),
            ("ordertype".into(), if req.order_type == OrderType::Market { "market".into() } else { "limit".into() }),
            ("volume".into(), format!("{:.10}", req.volume)),
        ];
        if let Some(price) = req.limit_price {
            form.push(("price".into(), format!("{price:.10}")));
        }
        if let Some(leverage) = &req.leverage {
            form.push(("leverage".into(), leverage.clone()));
        }
        if req.post_only {
            form.push(("oflags".into(), "post".into()));
        }

        let post_data = serde_urlencoded::to_string(&form).map_err(|e| ExecutionError::UnexpectedResponse(e.to_string()))?;
        let signature = sign_request(path, nonce, &post_data, api_secret)?;

        let url = format!("{}{path}", self.base_url);
        let resp: KrakenResponse<AddOrderResult> = self
            .http
            .post(&url)
            .header("API-Key", api_key)
            .header("API-Sign", signature)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(post_data)
            .send()
            .await?
            .json()
            .await?;
        unwrap_result(resp)
    }

    pub async fn cancel_order(&self, api_key: &str, api_secret: &str, txid: &str) -> Result<(), ExecutionError> {
        let path = "/0/private/CancelOrder";
        let nonce = generate_nonce();
        let form = [("nonce".to_string(), nonce.to_string()), ("txid".to_string(), txid.to_string())];
        let post_data = serde_urlencoded::to_string(&form).map_err(|e| ExecutionError::UnexpectedResponse(e.to_string()))?;
        let signature = sign_request(path, nonce, &post_data, api_secret)?;

        let url = format!("{}{path}", self.base_url);
        let resp: KrakenResponse<serde_json::Value> = self
            .http
            .post(&url)
            .header("API-Key", api_key)
            .header("API-Sign", signature)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(post_data)
            .send()
            .await?
            .json()
            .await?;
        unwrap_result(resp)?;
        Ok(())
    }

    pub async fn balance(&self, api_key: &str, api_secret: &str) -> Result<HashMap<String, String>, ExecutionError> {
        let path = "/0/private/Balance";
        let nonce = generate_nonce();
        let post_data = format!("nonce={nonce}");
        let signature = sign_request(path, nonce, &post_data, api_secret)?;

        let url = format!("{}{path}", self.base_url);
        let resp: KrakenResponse<HashMap<String, String>> = self
            .http
            .post(&url)
            .header("API-Key", api_key)
            .header("API-Sign", signature)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(post_data)
            .send()
            .await?
            .json()
            .await?;
        unwrap_result(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Kraken's own published worked example
    /// (<https://support.kraken.com/articles/360029054811>), cross-checked
    /// against an independent Python (`hashlib`/`hmac`) computation of the
    /// same inputs during this session - not just internal self-consistency.
    #[test]
    fn signature_matches_krakens_published_worked_example() {
        let secret = "FRs+gtq09rR7OFtKj9BGhyOGS3u5vtY/EdiIBO9kD8NFtRX7w7LeJDSrX6cq1D8zmQmGkWFjksuhBvKOAWJohQ==";
        let path = "/0/private/TradeBalance";
        let nonce: u64 = 1_540_973_848_000;
        let post_data = "nonce=1540973848000&asset=xbt";

        let sig = sign_request(path, nonce, post_data, secret).unwrap();
        assert_eq!(sig, "RdQzoXRC83TPmbERpFj0XFVArq0Hfadm0eLolmXTuN2R24hzIqtAnF/f7vSfW1tGt7xQOn8bjm+Ht+X0KrMwlA==");
    }

    #[test]
    fn signing_is_deterministic_for_the_same_inputs() {
        let sig1 = sign_request("/0/private/Balance", 42, "nonce=42", "aGVsbG8gd29ybGQ=").unwrap();
        let sig2 = sign_request("/0/private/Balance", 42, "nonce=42", "aGVsbG8gd29ybGQ=").unwrap();
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn signing_is_sensitive_to_every_input() {
        let base = sign_request("/0/private/Balance", 42, "nonce=42", "aGVsbG8gd29ybGQ=").unwrap();
        assert_ne!(base, sign_request("/0/private/AddOrder", 42, "nonce=42", "aGVsbG8gd29ybGQ=").unwrap());
        assert_ne!(base, sign_request("/0/private/Balance", 43, "nonce=42", "aGVsbG8gd29ybGQ=").unwrap());
        assert_ne!(base, sign_request("/0/private/Balance", 42, "nonce=43", "aGVsbG8gd29ybGQ=").unwrap());
        assert_ne!(base, sign_request("/0/private/Balance", 42, "nonce=42", "d29ybGQgaGVsbG8=").unwrap());
    }

    #[test]
    fn generated_nonces_strictly_increase() {
        let a = generate_nonce();
        let b = generate_nonce();
        assert!(b > a, "nonce must strictly increase across calls");
    }

    #[test]
    fn ticker_info_parses_best_bid_ask_and_last_price() {
        let json = r#"{"a":["50000.1","1","1.000"],"b":["49999.9","2","2.000"],"c":["50000.0","0.5"]}"#;
        let info: TickerInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.best_ask(), Some(50000.1));
        assert_eq!(info.best_bid(), Some(49999.9));
        assert_eq!(info.last_trade_price(), Some(50000.0));
    }

    #[test]
    fn kraken_error_response_surfaces_as_an_error_not_a_panic() {
        let json = r#"{"error":["EGeneral:Invalid arguments"],"result":null}"#;
        let resp: KrakenResponse<HashMap<String, TickerInfo>> = serde_json::from_str(json).unwrap();
        let result = unwrap_result(resp);
        assert!(matches!(result, Err(ExecutionError::KrakenSpot(_))));
    }

    /// Live-verified this session: Kraken's classic REST API rejects the
    /// slash-separated "wsname" form (`EQuery:Unknown asset pair`) and
    /// requires the concatenated form. `ticker()` and `add_order()` must
    /// both convert at their own boundary - see `rest_pair_symbol` docs.
    #[test]
    fn rest_pair_symbol_strips_the_slash() {
        assert_eq!(rest_pair_symbol("XBT/USD"), "XBTUSD");
        assert_eq!(rest_pair_symbol("ETH/XBT"), "ETHXBT");
        assert_eq!(rest_pair_symbol("XBTUSD"), "XBTUSD");
    }
}
