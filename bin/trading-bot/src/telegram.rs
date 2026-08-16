//! Optional Telegram alerts for fills, circuit-breaker trips, and errors.
//! Entirely best-effort: if `TELEGRAM_BOT_TOKEN`/`TELEGRAM_CHAT_ID` aren't
//! set, `TelegramClient::from_env` returns `None` and the alert task never
//! starts - the bot runs identically without it. Message formatting is
//! pure and unit-tested; actually delivering a message needs the user's
//! own bot token, which this sandbox doesn't have.

use bot_core::{AppEvent, LogLevel};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TelegramError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("telegram API error: {0}")]
    Api(String),
}

pub struct TelegramClient {
    http: reqwest::Client,
    bot_token: String,
    chat_id: String,
}

impl TelegramClient {
    /// Returns `None` (not an error) if either env var is unset or empty -
    /// Telegram alerts are opt-in, not a hard requirement.
    pub fn from_env() -> Option<Self> {
        let bot_token = non_empty_env("TELEGRAM_BOT_TOKEN")?;
        let chat_id = non_empty_env("TELEGRAM_CHAT_ID")?;
        Some(Self { http: reqwest::Client::new(), bot_token, chat_id })
    }

    pub async fn send_message(&self, text: &str) -> Result<(), TelegramError> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "chat_id": self.chat_id, "text": text, "parse_mode": "HTML" }))
            .send()
            .await?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(TelegramError::Api(body));
        }
        Ok(())
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.trim().is_empty())
}

/// Which `AppEvent`s become a Telegram message, and how they're worded.
/// Deliberately selective: fills, circuit-breaker state changes, and
/// errors are alert-worthy; routine rejections and discovery sightings
/// would be noise at any real trading frequency, so they're skipped here
/// (they're still fully logged/persisted - this only gates the *alert*).
pub fn format_event(event: &AppEvent) -> Option<String> {
    match event {
        AppEvent::Fill(fill) => Some(format!(
            "\u{1F4B0} <b>Fill</b> {:?} {:.4} {} @ {:.6} SOL{}",
            fill.side,
            fill.qty,
            fill.mint,
            fill.price,
            if fill.dry_run { " [DRY-RUN]" } else { "" }
        )),
        AppEvent::CircuitBreakerTripped { reason, .. } => {
            Some(format!("\u{1F6D1} <b>Circuit breaker tripped</b>: {reason}"))
        }
        AppEvent::CircuitBreakerReset { .. } => Some("\u{2705} Circuit breaker reset".to_string()),
        AppEvent::Log { level: LogLevel::Error, message, .. } => Some(format!("\u{26A0}\u{FE0F} {message}")),
        AppEvent::OrderRejected { .. }
        | AppEvent::TokenDiscovered(_)
        | AppEvent::TokenRejectedBySafety { .. }
        | AppEvent::Log { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::{OrderReason, Pubkey, Side};

    #[test]
    fn fill_is_formatted_with_side_and_price() {
        let fill = bot_core::Fill {
            mint: Pubkey::new_unique(), side: Side::Buy, qty: 5.0, price: 1.23, sol_amount: 6.15,
            fee_sol: 0.0, jito_tip_sol: 0.0, slippage_bps: None, strategy: "momentum".into(),
            reason: OrderReason::Strategy, tx_signature: None, bundle_id: None, dry_run: true, ts: 0,
        };
        let msg = format_event(&AppEvent::Fill(fill)).unwrap();
        assert!(msg.contains("Buy"));
        assert!(msg.contains("1.23"));
        assert!(msg.contains("DRY-RUN"));
    }

    #[test]
    fn circuit_breaker_trip_and_reset_are_formatted() {
        let trip = format_event(&AppEvent::CircuitBreakerTripped { reason: "daily loss".into(), ts: 0 }).unwrap();
        assert!(trip.contains("daily loss"));
        let reset = format_event(&AppEvent::CircuitBreakerReset { ts: 0 }).unwrap();
        assert!(reset.contains("reset"));
    }

    #[test]
    fn error_level_logs_are_alerted_but_info_logs_are_not() {
        let err = format_event(&AppEvent::Log { level: LogLevel::Error, message: "boom".into(), ts: 0 });
        assert!(err.is_some());
        let info = format_event(&AppEvent::Log { level: LogLevel::Info, message: "fyi".into(), ts: 0 });
        assert!(info.is_none());
    }

    #[test]
    fn routine_rejections_and_discovery_are_not_alerted() {
        assert!(format_event(&AppEvent::OrderRejected { mint: Pubkey::new_unique(), reason: "no capital".into() })
            .is_none());
        assert!(format_event(&AppEvent::TokenRejectedBySafety {
            mint: Pubkey::new_unique(),
            reasons: vec!["low liquidity".into()]
        })
        .is_none());
    }

    #[test]
    fn from_env_is_none_without_credentials() {
        // SAFETY: test-only env var manipulation, scoped to this process;
        // no other test in this crate reads these keys concurrently.
        unsafe {
            std::env::remove_var("TELEGRAM_BOT_TOKEN");
            std::env::remove_var("TELEGRAM_CHAT_ID");
        }
        assert!(TelegramClient::from_env().is_none());
    }
}
