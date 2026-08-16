//! Jupiter Swap API v2 client (`lite-api.jup.ag`) - confirmed live and
//! keyless this session (`GET /swap/v1/quote` returned a real 0.1 SOL ->
//! USDC quote). Used for any `TokenPhase::Migrated` order; Jupiter has
//! routed PumpSwap pools since March 2025, so this already covers most
//! post-graduation memecoin trading without a separate PumpSwap client.
//!
//! Hand-rolled rather than a third-party crate: `jupiter-swap-api-client`'s
//! crates.io publish lags its GitHub repo by roughly a year against an API
//! that evolves fast - two endpoints are simple enough to own directly.

use bot_core::Pubkey;
use serde::Deserialize;
use serde_json::Value;

use crate::error::ExecutionError;

const DEFAULT_BASE_URL: &str = "https://lite-api.jup.ag";

pub struct JupiterClient {
    http: reqwest::Client,
    base_url: String,
}

/// A parsed quote. `raw` is kept because Jupiter's `/swap` endpoint expects
/// the *entire* quote response echoed back verbatim - re-serializing a
/// hand-rolled subset risks silently dropping fields Jupiter needs.
#[derive(Debug, Clone)]
pub struct QuoteResponse {
    pub raw: Value,
    pub in_amount: u64,
    pub out_amount: u64,
    pub price_impact_pct: f64,
}

#[derive(Debug, Deserialize)]
struct SwapResponse {
    #[serde(rename = "swapTransaction")]
    swap_transaction: String,
}

impl JupiterClient {
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    pub fn with_base_url(base_url: &str) -> Self {
        Self { http: reqwest::Client::new(), base_url: base_url.to_string() }
    }

    /// `GET /swap/v1/quote` - the real, keyless, confirmed-live endpoint.
    pub async fn quote(
        &self,
        input_mint: &Pubkey,
        output_mint: &Pubkey,
        amount: u64,
        slippage_bps: u16,
    ) -> Result<QuoteResponse, ExecutionError> {
        let url = format!(
            "{}/swap/v1/quote?inputMint={}&outputMint={}&amount={}&slippageBps={}",
            self.base_url, input_mint, output_mint, amount, slippage_bps
        );
        let resp = self.http.get(&url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ExecutionError::Jupiter(format!("quote failed ({status}): {body}")));
        }
        let value: Value = resp.json().await?;
        parse_quote_response(value)
    }

    /// `POST /swap/v1/swap` - builds the unsigned swap transaction for a
    /// previously-fetched quote. Only reached outside `dry_run` mode.
    pub async fn swap_transaction_base64(
        &self,
        quote: &QuoteResponse,
        user_pubkey: &Pubkey,
    ) -> Result<String, ExecutionError> {
        let url = format!("{}/swap/v1/swap", self.base_url);
        let body = serde_json::json!({
            "quoteResponse": quote.raw,
            "userPublicKey": user_pubkey.to_string(),
            "wrapAndUnwrapSol": true,
        });
        let resp = self.http.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ExecutionError::Jupiter(format!("swap build failed ({status}): {text}")));
        }
        let parsed: SwapResponse = resp.json().await?;
        Ok(parsed.swap_transaction)
    }
}

impl Default for JupiterClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Pulled out as a pure function so it's testable against recorded fixture
/// JSON without a network call.
fn parse_quote_response(value: Value) -> Result<QuoteResponse, ExecutionError> {
    let in_amount: u64 = value
        .get("inAmount")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| ExecutionError::UnexpectedResponse("missing/invalid inAmount".into()))?;
    let out_amount: u64 = value
        .get("outAmount")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| ExecutionError::UnexpectedResponse("missing/invalid outAmount".into()))?;
    let price_impact_pct: f64 = value
        .get("priceImpactPct")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);

    Ok(QuoteResponse { raw: value, in_amount, out_amount, price_impact_pct })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed but real-shaped Jupiter `/quote` response (field names and
    /// types match what was observed from the live endpoint this session).
    const FIXTURE_QUOTE: &str = r#"{
        "inputMint": "So11111111111111111111111111111111111111112",
        "inAmount": "100000000",
        "outputMint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
        "outAmount": "7518602",
        "otherAmountThreshold": "7443416",
        "swapMode": "ExactIn",
        "slippageBps": 100,
        "priceImpactPct": "0.0012",
        "routePlan": []
    }"#;

    #[test]
    fn parses_a_real_shaped_quote_fixture() {
        let value: Value = serde_json::from_str(FIXTURE_QUOTE).unwrap();
        let quote = parse_quote_response(value).unwrap();
        assert_eq!(quote.in_amount, 100_000_000);
        assert_eq!(quote.out_amount, 7_518_602);
        assert!((quote.price_impact_pct - 0.0012).abs() < 1e-9);
    }

    #[test]
    fn rejects_a_response_missing_out_amount() {
        let value: Value = serde_json::json!({ "inAmount": "100" });
        assert!(parse_quote_response(value).is_err());
    }

    #[test]
    fn raw_quote_is_preserved_verbatim_for_the_swap_call() {
        let value: Value = serde_json::from_str(FIXTURE_QUOTE).unwrap();
        let quote = parse_quote_response(value.clone()).unwrap();
        assert_eq!(quote.raw, value);
    }
}
