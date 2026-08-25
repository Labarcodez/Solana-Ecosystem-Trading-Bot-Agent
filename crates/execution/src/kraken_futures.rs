//! Kraken Futures (perpetuals) REST client. A genuinely separate product
//! from Spot: different host, different auth scheme, and a funding-rate
//! mechanism spot/margin don't have. See module docs on `kraken_spot` for
//! the general hand-rolled-vs-crate rationale, which applies equally here.
//!
//! **Verification tier:** the signing algorithm (`sign_futures_request`) is
//! implemented from Kraken's official Futures REST auth guide
//! (<https://docs.kraken.com/api/docs/guides/futures-rest/>), including the
//! February 2024 URL-encoding change and the exact `/api/v3/<endpoint>`
//! path form used inside the hash (confirmed via that page, not guessed).
//! Cross-checked in `tests::signature_is_internally_verifiable_via_independent_python_computation`
//! against an independent Python (`hashlib`/`hmac`) computation of the same
//! made-up-but-representative inputs - Kraken's docs did not include a
//! full numeric worked example the way the Spot auth page does, so this is
//! a self-consistency cross-check, not a match against an official
//! published vector (an honest, weaker tier than `kraken_spot`'s, and
//! documented as such). Response field names (`fundingRate` etc.) are taken
//! from public API documentation and have not been exercised against a
//! live Futures account in this sandbox - no funded API key was available.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256, Sha512};

use crate::error::ExecutionError;

const DEFAULT_BASE_URL: &str = "https://futures.kraken.com/derivatives/api/v3";

/// `HMAC-SHA512(secret, SHA256(postdata + nonce + endpoint))`, base64
/// encoded. `endpoint` must be the short `/api/v3/<name>` form used inside
/// the hash, **not** the full request URL (see module docs) - callers pass
/// the same short form both here and in the request path.
pub fn sign_futures_request(
    endpoint: &str,
    nonce: &str,
    post_data: &str,
    api_secret_b64: &str,
) -> Result<String, ExecutionError> {
    let secret = STANDARD.decode(api_secret_b64).map_err(|e| ExecutionError::Base64(e.to_string()))?;

    let mut sha256 = Sha256::new();
    sha256.update(post_data.as_bytes());
    sha256.update(nonce.as_bytes());
    sha256.update(endpoint.as_bytes());
    let digest = sha256.finalize();

    let mut mac = Hmac::<Sha512>::new_from_slice(&secret)
        .map_err(|e| ExecutionError::KrakenFutures(format!("invalid API secret: {e}")))?;
    mac.update(&digest);
    let signature = mac.finalize().into_bytes();

    Ok(STANDARD.encode(signature))
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct FuturesTicker {
    pub symbol: String,
    #[serde(default)]
    pub last: Option<f64>,
    #[serde(default)]
    pub bid: Option<f64>,
    #[serde(default)]
    pub ask: Option<f64>,
    /// Hourly funding rate, if this symbol is a perpetual. `None` for
    /// fixed-expiry futures.
    #[serde(rename = "fundingRate", default)]
    pub funding_rate: Option<f64>,
}

#[derive(Debug, serde::Deserialize)]
struct FuturesTickersResponse {
    result: String,
    #[serde(default)]
    tickers: Vec<FuturesTicker>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct FuturesSendOrderResult {
    #[serde(default)]
    pub order_id: Option<String>,
    pub status: String,
}

#[derive(Debug, serde::Deserialize)]
struct FuturesSendOrderResponse {
    result: String,
    #[serde(rename = "sendStatus", default)]
    send_status: Option<FuturesSendOrderResult>,
    #[serde(default)]
    error: Option<String>,
}

fn check_result(result: &str, error: Option<String>) -> Result<(), ExecutionError> {
    if result != "success" {
        return Err(ExecutionError::KrakenFutures(error.unwrap_or_else(|| result.to_string())));
    }
    Ok(())
}

pub struct FuturesOrderRequest<'a> {
    pub symbol: &'a str,
    pub side: &'a str, // "buy" | "sell"
    pub market: bool,  // true = "mkt", false = "lmt"
    pub limit_price: Option<f64>,
    pub size: f64,
}

pub struct KrakenFuturesClient {
    http: reqwest::Client,
    base_url: String,
}

impl Default for KrakenFuturesClient {
    fn default() -> Self {
        Self::new()
    }
}

impl KrakenFuturesClient {
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self { http: reqwest::Client::new(), base_url: base_url.into() }
    }

    /// Real, keyless public endpoint - includes each perpetual's current
    /// funding rate, which is what `risk`'s funding-bleed circuit-breaker
    /// check and the `funding_carry` strategy both read.
    pub async fn tickers(&self) -> Result<Vec<FuturesTicker>, ExecutionError> {
        let url = format!("{}/tickers", self.base_url);
        let resp: FuturesTickersResponse = self.http.get(&url).send().await?.json().await?;
        check_result(&resp.result, resp.error)?;
        Ok(resp.tickers)
    }

    pub async fn send_order(
        &self,
        api_key: &str,
        api_secret: &str,
        req: &FuturesOrderRequest<'_>,
    ) -> Result<FuturesSendOrderResult, ExecutionError> {
        let endpoint = "/api/v3/sendorder";
        let nonce = super::kraken_spot::generate_nonce().to_string();

        let mut form: Vec<(String, String)> = vec![
            ("orderType".into(), if req.market { "mkt".into() } else { "lmt".into() }),
            ("symbol".into(), req.symbol.to_string()),
            ("side".into(), req.side.to_string()),
            ("size".into(), format!("{:.10}", req.size)),
        ];
        if let Some(price) = req.limit_price {
            form.push(("limitPrice".into(), format!("{price:.10}")));
        }
        let post_data = serde_urlencoded::to_string(&form).map_err(|e| ExecutionError::UnexpectedResponse(e.to_string()))?;
        let signature = sign_futures_request(endpoint, &nonce, &post_data, api_secret)?;

        let url = format!("{}/sendorder", self.base_url);
        let resp: FuturesSendOrderResponse = self
            .http
            .post(&url)
            .header("APIKey", api_key)
            .header("Nonce", &nonce)
            .header("Authent", signature)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(post_data)
            .send()
            .await?
            .json()
            .await?;
        check_result(&resp.result, resp.error)?;
        resp.send_status.ok_or_else(|| ExecutionError::UnexpectedResponse("missing sendStatus".into()))
    }

    pub async fn cancel_order(&self, api_key: &str, api_secret: &str, order_id: &str) -> Result<(), ExecutionError> {
        let endpoint = "/api/v3/cancelorder";
        let nonce = super::kraken_spot::generate_nonce().to_string();
        let form = [("order_id".to_string(), order_id.to_string())];
        let post_data = serde_urlencoded::to_string(&form).map_err(|e| ExecutionError::UnexpectedResponse(e.to_string()))?;
        let signature = sign_futures_request(endpoint, &nonce, &post_data, api_secret)?;

        let url = format!("{}/cancelorder", self.base_url);
        let resp: FuturesSendOrderResponse = self
            .http
            .post(&url)
            .header("APIKey", api_key)
            .header("Nonce", &nonce)
            .header("Authent", signature)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(post_data)
            .send()
            .await?
            .json()
            .await?;
        check_result(&resp.result, resp.error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not an official Kraken-published vector (unlike `kraken_spot`'s) -
    /// Kraken's Futures auth guide describes the algorithm precisely but
    /// doesn't include a full numeric worked example. This cross-checks the
    /// Rust implementation against an independent Python computation of the
    /// same representative inputs, done during this session - see module
    /// docs for the honest verification-tier distinction from `kraken_spot`.
    #[test]
    fn signature_is_internally_verifiable_via_independent_python_computation() {
        let secret = "FRs+gtq09rR7OFtKj9BGhyOGS3u5vtY/EdiIBO9kD8NFtRX7w7LeJDSrX6cq1D8zmQmGkWFjksuhBvKOAWJohQ==";
        let endpoint = "/api/v3/sendorder";
        let nonce = "1616492376594";
        let post_data = "orderType=lmt&symbol=pi_xbtusd&side=buy&size=1&limitPrice=10000";

        let sig = sign_futures_request(endpoint, nonce, post_data, secret).unwrap();
        assert_eq!(sig, "WJA8+ogOlH40r6lA8wrc4Lh0Qaf5zvSEQAnRhbyT+8NPntHISUK5OeiJumBOXHXvVljZ3+wq+HFI8qx6AysLLA==");
    }

    #[test]
    fn signing_is_sensitive_to_every_input() {
        let base = sign_futures_request("/api/v3/sendorder", "1", "a=b", "aGVsbG8gd29ybGQ=").unwrap();
        assert_ne!(base, sign_futures_request("/api/v3/cancelorder", "1", "a=b", "aGVsbG8gd29ybGQ=").unwrap());
        assert_ne!(base, sign_futures_request("/api/v3/sendorder", "2", "a=b", "aGVsbG8gd29ybGQ=").unwrap());
        assert_ne!(base, sign_futures_request("/api/v3/sendorder", "1", "a=c", "aGVsbG8gd29ybGQ=").unwrap());
    }

    #[test]
    fn ticker_parses_funding_rate_when_present() {
        let json = r#"{"symbol":"PI_XBTUSD","last":50000.0,"bid":49999.0,"ask":50001.0,"fundingRate":0.0001}"#;
        let ticker: FuturesTicker = serde_json::from_str(json).unwrap();
        assert_eq!(ticker.symbol, "PI_XBTUSD");
        assert_eq!(ticker.funding_rate, Some(0.0001));
    }

    #[test]
    fn ticker_without_funding_rate_parses_as_none_not_an_error() {
        // Fixed-expiry futures don't have a funding rate.
        let json = r#"{"symbol":"FI_XBTUSD_240628","last":51000.0}"#;
        let ticker: FuturesTicker = serde_json::from_str(json).unwrap();
        assert_eq!(ticker.funding_rate, None);
    }

    #[test]
    fn error_result_surfaces_as_an_error_not_a_panic() {
        let json = r#"{"result":"error","error":"insufficientFunds"}"#;
        let resp: FuturesTickersResponse = serde_json::from_str(json).unwrap();
        let outcome = check_result(&resp.result, resp.error);
        assert!(matches!(outcome, Err(ExecutionError::KrakenFutures(msg)) if msg == "insufficientFunds"));
    }
}
