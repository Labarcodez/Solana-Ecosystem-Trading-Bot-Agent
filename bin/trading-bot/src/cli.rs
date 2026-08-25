use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "trading-bot", about = "Local, terminal-based Kraken multi-strategy trading bot")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the trading bot
    Run(RunArgs),
    /// Manage encrypted Kraken API credentials
    Credentials {
        #[command(subcommand)]
        action: CredentialsAction,
    },
}

#[derive(Subcommand)]
pub enum CredentialsAction {
    /// Prompt for a Kraken API key/secret and write them to an encrypted file
    Init {
        /// Defaults to $KRAKEN_CREDENTIALS_PATH (or
        /// $KRAKEN_FUTURES_CREDENTIALS_PATH with --futures) from .env, or
        /// ./kraken.enc.json (./kraken_futures.enc.json).
        #[arg(long)]
        path: Option<PathBuf>,

        /// Initialize the Futures-specific credentials file instead of the
        /// Spot/Margin one. Kraken recommends a separate API key per
        /// product - keeping them in separate files limits the blast
        /// radius if one key is ever compromised.
        #[arg(long)]
        futures: bool,
    },
}

#[derive(Args)]
pub struct RunArgs {
    #[arg(long, default_value = "config/config.toml")]
    pub config: PathBuf,

    /// Overrides [general].mode ("dry_run" | "live") from the config file.
    #[arg(long)]
    pub mode: Option<String>,

    /// "mock" replays data/sample_xbtusd.csv (real Kraken hourly OHLC
    /// close data) with zero external API keys. "live" streams real prices
    /// via Kraken's public WebSocket v2 (`[kraken].pairs`) and/or polls
    /// Kraken Futures tickers (`[kraken].futures_pairs`) - both public,
    /// keyless endpoints.
    #[arg(long, default_value = "mock")]
    pub price_source: String,

    /// CSV path for --price-source mock.
    #[arg(long, default_value = "data/sample_xbtusd.csv")]
    pub mock_data: PathBuf,

    /// Milliseconds between mock ticks. 0 replays as fast as possible.
    #[arg(long, default_value_t = 0)]
    pub mock_tick_interval_ms: u64,

    #[arg(long, default_value = "./trading_bot.db")]
    pub db_path: String,

    /// Paper-trading starting capital (in the configured quote currency)
    /// for dry-run/mock mode. Ignored in live mode, where the account's
    /// real Kraken balance is fetched instead.
    #[arg(long, default_value_t = 10_000.0)]
    pub starting_capital_quote: f64,

    /// Run headless (no Ratatui dashboard) - plain log lines instead.
    #[arg(long)]
    pub no_tui: bool,
}
