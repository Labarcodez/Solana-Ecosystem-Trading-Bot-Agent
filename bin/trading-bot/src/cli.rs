use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "trading-bot", about = "Local, terminal-based Solana multi-strategy trading bot")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the trading bot
    Run(RunArgs),
    /// Manage the encrypted wallet key
    Wallet {
        #[command(subcommand)]
        action: WalletAction,
    },
}

#[derive(Subcommand)]
pub enum WalletAction {
    /// Generate a new keypair and write it to an encrypted key file
    Init {
        /// Defaults to $WALLET_KEY_PATH from .env, or ./wallet.enc.json
        #[arg(long)]
        path: Option<PathBuf>,
    },
}

#[derive(Args)]
pub struct RunArgs {
    #[arg(long, default_value = "config/config.toml")]
    pub config: PathBuf,

    /// Overrides [general].mode ("dry_run" | "live") from the config file.
    #[arg(long)]
    pub mode: Option<String>,

    /// "mock" replays data/sample_sol_usdc.csv with zero external API keys.
    /// "live" streams real prices via Shyft's Yellowstone gRPC (requires
    /// SHYFT_GRPC_ENDPOINT/SHYFT_X_TOKEN in .env).
    #[arg(long, default_value = "mock")]
    pub price_source: String,

    /// CSV path for --price-source mock.
    #[arg(long, default_value = "data/sample_sol_usdc.csv")]
    pub mock_data: PathBuf,

    /// Milliseconds between mock ticks. 0 replays as fast as possible.
    #[arg(long, default_value_t = 0)]
    pub mock_tick_interval_ms: u64,

    #[arg(long, default_value = "./trading_bot.db")]
    pub db_path: String,

    /// Paper-trading starting capital in dry-run/mock mode. Ignored in
    /// live mode, where the wallet's real on-chain SOL balance is used
    /// instead.
    #[arg(long, default_value_t = 10.0)]
    pub starting_capital_sol: f64,

    /// Run headless (no Ratatui dashboard) - plain log lines instead.
    #[arg(long)]
    pub no_tui: bool,
}
