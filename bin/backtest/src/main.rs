//! `backtest`: replay historical price data through a strategy using the
//! exact same `Strategy`/`RiskManager` code the live `trading-bot` binary
//! uses. Reads the same `config.toml` the live bot reads.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context;
use bot_core::RiskTier;
use clap::Parser;
use risk::{RiskConfig, TierConfig};
use serde::Deserialize;
use strategies::{FundingCarryConfig, GridConfig, MarketMakerConfig, MomentumConfig, TriangularArbitrageConfig};

#[derive(Parser)]
#[command(name = "backtest", about = "Replay historical price data through a trading strategy")]
struct Cli {
    /// Path to the shared config.toml (same file the live bot reads).
    #[arg(long, default_value = "config/config.toml")]
    config: PathBuf,

    /// Override [general].strategy from the config file.
    #[arg(long)]
    strategy: Option<String>,

    /// Override [backtest].data_file from the config file.
    #[arg(long)]
    data: Option<PathBuf>,

    /// Also print the full report as JSON to stdout.
    #[arg(long)]
    json: bool,

    /// Where to write the JSON report.
    #[arg(long, default_value = "backtest_report.json")]
    out: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
struct GeneralSection {
    strategy: String,
}

#[derive(Debug, Clone, Deserialize)]
struct StrategySection {
    momentum: MomentumConfig,
    grid: GridConfig,
    market_maker: MarketMakerConfig,
    triangular_arbitrage: TriangularArbitrageConfig,
    funding_carry: FundingCarryConfig,
}

/// Mirrors `[risk]` including its nested `[risk.tiers.*]` sub-tables. Split
/// into `RiskConfig` + a `RiskTier`-keyed tier map by [`into_parts`].
#[derive(Debug, Clone, Deserialize)]
struct RiskSection {
    max_open_positions: usize,
    daily_loss_limit_quote: f64,
    daily_loss_limit_pct: f64,
    default_stop_loss_pct: f64,
    default_take_profit_pct: f64,
    tiers: HashMap<String, TierConfig>,
}

impl RiskSection {
    fn into_parts(self) -> (RiskConfig, HashMap<RiskTier, TierConfig>) {
        let risk_cfg = RiskConfig {
            max_open_positions: self.max_open_positions,
            daily_loss_limit_quote: self.daily_loss_limit_quote,
            daily_loss_limit_pct: self.daily_loss_limit_pct,
            default_stop_loss_pct: self.default_stop_loss_pct,
            default_take_profit_pct: self.default_take_profit_pct,
        };
        let mut tiers = HashMap::new();
        for (name, cfg) in self.tiers {
            let tier = match name.as_str() {
                "spot" => RiskTier::Spot,
                "margin" => RiskTier::Margin,
                "futures" => RiskTier::Futures,
                other => {
                    eprintln!("warning: ignoring unknown [risk.tiers.{other}] section in config");
                    continue;
                }
            };
            tiers.insert(tier, cfg);
        }
        (risk_cfg, tiers)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RootConfig {
    general: GeneralSection,
    strategy: StrategySection,
    risk: RiskSection,
    backtest: backtester::BacktestConfig,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let config_str = std::fs::read_to_string(&cli.config)
        .with_context(|| format!("reading config file {}", cli.config.display()))?;
    let root: RootConfig = toml::from_str(&config_str)
        .with_context(|| format!("parsing config file {}", cli.config.display()))?;

    let strategy_name = cli.strategy.unwrap_or_else(|| root.general.strategy.clone());
    let data_path = cli.data.unwrap_or_else(|| PathBuf::from(&root.backtest.data_file));
    let (risk_cfg, tiers) = root.risk.into_parts();

    if tiers.is_empty() {
        anyhow::bail!(
            "no valid [risk.tiers.*] sections found in {} - expected spot, margin, futures",
            cli.config.display()
        );
    }

    let params = backtester::BacktestParams {
        strategy_name: &strategy_name,
        momentum: &root.strategy.momentum,
        grid: &root.strategy.grid,
        market_maker: &root.strategy.market_maker,
        triangular_arbitrage: &root.strategy.triangular_arbitrage,
        funding_carry: &root.strategy.funding_carry,
        risk: &risk_cfg,
        tiers: &tiers,
        backtest: &root.backtest,
        data_path: &data_path,
    };

    println!("Running backtest: strategy={strategy_name} data={}", data_path.display());
    let report = backtester::run(&params).with_context(|| "running backtest")?;

    report.print_summary();

    let json = report.to_json_pretty()?;
    std::fs::write(&cli.out, &json).with_context(|| format!("writing report to {}", cli.out.display()))?;
    println!("Full report written to {}", cli.out.display());

    if cli.json {
        println!("{json}");
    }

    Ok(())
}
