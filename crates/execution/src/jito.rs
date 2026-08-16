//! Jito Block Engine client - confirmed live and keyless this session
//! (`getTipAccounts` returned 8 real tip account pubkeys). Bundles land
//! transactions atomically and MEV-protected; submission is free, landing
//! costs a small SOL tip paid to one of the returned tip accounts.
//!
//! Hand-rolled JSON-RPC rather than `jito-sdk-rust`: the API is two
//! `sendBundle`/`getTipAccounts`-shaped calls, and the crate's last publish
//! (0.3.2, ~8 months old at research time) is a staleness risk not worth
//! taking on for this little surface area.

use bot_core::Pubkey;
use serde_json::{json, Value};

use crate::error::ExecutionError;

const DEFAULT_BLOCK_ENGINE_URL: &str = "https://mainnet.block-engine.jito.wtf";

pub struct JitoClient {
    http: reqwest::Client,
    block_engine_url: String,
}

impl JitoClient {
    pub fn new() -> Self {
        Self::with_block_engine_url(DEFAULT_BLOCK_ENGINE_URL)
    }

    pub fn with_block_engine_url(url: &str) -> Self {
        Self { http: reqwest::Client::new(), block_engine_url: url.to_string() }
    }

    pub async fn get_tip_accounts(&self) -> Result<Vec<Pubkey>, ExecutionError> {
        let url = format!("{}/api/v1/bundles", self.block_engine_url);
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "getTipAccounts", "params": [] });
        let resp = self.http.post(&url).json(&body).send().await?;
        let value: Value = resp.json().await?;
        parse_tip_accounts(&value)
    }

    /// Submits a bundle: base58-encoded, fully-signed transactions, tip
    /// transaction first. Max 5 transactions per Jito's own limit. Returns
    /// the bundle id used to poll `getBundleStatuses` afterward.
    pub async fn send_bundle(&self, signed_txs_base58: Vec<String>) -> Result<String, ExecutionError> {
        let url = format!("{}/api/v1/bundles", self.block_engine_url);
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "sendBundle", "params": [signed_txs_base58] });
        let resp = self.http.post(&url).json(&body).send().await?;
        let value: Value = resp.json().await?;
        parse_send_bundle_response(&value)
    }

    pub async fn get_bundle_statuses(&self, bundle_ids: Vec<String>) -> Result<Value, ExecutionError> {
        let url = format!("{}/api/v1/bundles", self.block_engine_url);
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "getBundleStatuses", "params": [bundle_ids] });
        let resp = self.http.post(&url).json(&body).send().await?;
        let value: Value = resp.json().await?;
        if let Some(err) = value.get("error") {
            return Err(ExecutionError::Jito(err.to_string()));
        }
        Ok(value)
    }
}

impl Default for JitoClient {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_tip_accounts(value: &Value) -> Result<Vec<Pubkey>, ExecutionError> {
    if let Some(err) = value.get("error") {
        return Err(ExecutionError::Jito(err.to_string()));
    }
    let result = value
        .get("result")
        .and_then(Value::as_array)
        .ok_or_else(|| ExecutionError::UnexpectedResponse("missing result array".into()))?;
    result
        .iter()
        .map(|v| {
            v.as_str()
                .ok_or_else(|| ExecutionError::UnexpectedResponse("tip account not a string".into()))
                .and_then(|s| s.parse::<Pubkey>().map_err(|e| ExecutionError::InvalidPubkey(e.to_string())))
        })
        .collect()
}

fn parse_send_bundle_response(value: &Value) -> Result<String, ExecutionError> {
    if let Some(err) = value.get("error") {
        return Err(ExecutionError::Jito(err.to_string()));
    }
    value
        .get("result")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .ok_or_else(|| ExecutionError::UnexpectedResponse("missing bundle id in result".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real-shaped response: 8 tip accounts, matching what was observed
    /// live from `getTipAccounts` this session.
    const FIXTURE_TIP_ACCOUNTS: &str = r#"{
        "jsonrpc": "2.0",
        "result": [
            "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
            "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
            "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
            "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
            "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
            "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
            "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
            "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT"
        ],
        "id": 1
    }"#;

    const FIXTURE_SEND_BUNDLE: &str = r#"{"jsonrpc":"2.0","result":"a1b2c3d4-bundle-id","id":1}"#;
    const FIXTURE_ERROR: &str = r#"{"jsonrpc":"2.0","error":{"code":-32602,"message":"invalid params"},"id":1}"#;

    #[test]
    fn parses_real_shaped_tip_accounts_fixture() {
        let value: Value = serde_json::from_str(FIXTURE_TIP_ACCOUNTS).unwrap();
        let accounts = parse_tip_accounts(&value).unwrap();
        assert_eq!(accounts.len(), 8);
    }

    #[test]
    fn parses_send_bundle_response() {
        let value: Value = serde_json::from_str(FIXTURE_SEND_BUNDLE).unwrap();
        let bundle_id = parse_send_bundle_response(&value).unwrap();
        assert_eq!(bundle_id, "a1b2c3d4-bundle-id");
    }

    #[test]
    fn surfaces_jsonrpc_errors_instead_of_panicking() {
        let value: Value = serde_json::from_str(FIXTURE_ERROR).unwrap();
        assert!(parse_tip_accounts(&value).is_err());
        assert!(parse_send_bundle_response(&value).is_err());
    }
}
