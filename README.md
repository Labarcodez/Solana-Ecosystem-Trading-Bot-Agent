# Solana Ecosystem Trading Bot

A **local, terminal-based Solana trading bot** that runs entirely on your own
machine. No cloud hosting, no third party ever touches your wallet key. It's
a **multi-strategy framework**: a shared core (market data, execution, risk
management, dashboard) that pluggable strategies (momentum, grid, more you
write later) trade against.

The bot watches Solana token prices in real time, runs your chosen strategy
against that data, manages risk automatically (position sizing, stop-loss/
take-profit, a daily-loss circuit breaker), executes trades through Jupiter
with Jito MEV-protected landing, logs every trade to a local SQLite database,
and displays it all in a live terminal dashboard. It also includes a
backtesting engine so you can test a strategy against real historical data
before risking anything.

It's built to trade **anything in the Solana ecosystem, including brand-new
memecoins** — both tokens still on pump.fun's bonding curve (pre-graduation)
and tokens that have already migrated to a real AMM pool — gated behind
mandatory, rule-based safety checks and risk sizing that's automatically
more conservative the less is known about a token. See
[Honesty notes](#honesty-notes--known-limitations) below for exactly what
that means in practice.

Everything runs at **$0 cost** using free tiers (Alchemy RPC, Shyft
Yellowstone gRPC, Jupiter Swap API, Jito's free bundle submission). The only
real cost is on-chain gas, priority fees, and Jito tips — your own trading
capital being spent, not a service fee.

## Status: what actually works right now

Every crate below compiles, is unit-tested, and passes clippy with zero
warnings. **121 tests, all passing.** The full pipeline has been run
end-to-end in `--mode dry_run --price-source mock`, including a real
momentum-strategy buy-then-sell round trip against **live** Jupiter `/quote`
calls (no fake data, no mocked HTTP responses at the integration level).

Read [Honesty notes](#honesty-notes--known-limitations) before you point
this at real funds — it explains precisely what's fully wired for live
trading today versus what's real-but-not-yet-connected.

## Architecture

```
discovery ──TokenDiscovered──▶ safety (scores/filters) ──▶ market_data
  (pump.fun create/graduate)                                (adds mint to watched set only if it passes)

market_data ──PriceTick (broadcast)──┬─▶ strategy_engine
                                      ├─▶ risk_manager (SL/TP fires every tick, not signal-gated)
                                      ├─▶ tui
                                      └─▶ storage
strategy_engine ──Signal (mpsc)──▶ risk_manager (single veto point before execution)
risk_manager    ──ApprovedOrder──▶ executor (routes by TokenMeta.phase:
                                              bonding_curve → pump.fun client
                                              migrated       → Jupiter + Jito)
executor        ──AppEvent (broadcast)──┬─▶ storage
  (Fill / Rejected / CircuitBreaker)    ├─▶ tui
                                        ├─▶ risk_manager (feedback: PnL/positions)
                                        └─▶ telegram (optional)
```

A newly-seen mint only ever becomes tradable after `safety` clears it —
nothing downstream can bypass that gate. `risk` is the single point between
a strategy's signal and the executor: no code path reaches `execution`
without going through it first.

### Workspace layout

| Crate | Responsibility |
|---|---|
| `crates/core` (`bot-core`) | Domain types (`PriceTick`, `Signal`, `Fill`, `TokenMeta`, `TrustTier`, ...) + the `Strategy` trait. Zero I/O dependencies. |
| `crates/strategies` | `MomentumStrategy` (SMA crossover) and `GridStrategy`, plus the `build_strategy()` factory both binaries use — this is what guarantees live and backtest runs execute identical strategy logic. |
| `crates/risk` | `RiskManager`: per-trust-tier position sizing, stop-loss/take-profit, the daily-loss circuit breaker. |
| `crates/wallet` | Argon2id + AES-256-GCM encrypted keypair, interactive passphrase prompts. |
| `crates/market_data` | Real Yellowstone gRPC price streaming (vault-ratio pricing, works across any constant-product AMM) + a CSV mock-replay source. |
| `crates/discovery` | Real-time pump.fun `create`/graduation watcher via Yellowstone gRPC. |
| `crates/safety` | `TokenSafetyScorer` trait + `RuleBasedScorer` (mint/freeze authority, LP burn, holder concentration, liquidity, age). |
| `crates/execution` | Jupiter (quote/swap), Jito (bundle landing), and a direct pump.fun bonding-curve client, routed by token phase. |
| `crates/storage` | SQLite trade/position/equity/event log, on its own dedicated thread. |
| `crates/tui` | The Ratatui dashboard. |
| `crates/backtester` | Replays historical data through the *same* `Strategy`/`RiskManager` code the live bot uses. |
| `bin/trading-bot` | The live binary — wires everything above together. |
| `bin/backtest` | Thin CLI around `crates/backtester`. |

## Quick start

Requires a stable Rust toolchain (see `rust-toolchain.toml`).

```bash
# 1. Build everything
cargo build --workspace

# 2. Run the test suite
cargo test --workspace

# 3. Backtest a strategy against the bundled real historical data
cargo run --bin backtest -- --config config/config.toml

# 4. Run the full live-shaped pipeline with zero API keys and zero cost:
#    replays real historical SOL/USD data, runs your strategy, sizes and
#    "fills" trades via a REAL Jupiter /quote call, but never signs or
#    submits anything on-chain.
cargo run --bin trading-bot -- run --price-source mock --no-tui
# drop --no-tui to see the live dashboard (needs a real terminal)
```

### Going live

1. `cargo run --bin trading-bot -- wallet init` — generates a new keypair,
   prompts for a passphrase (twice, never echoed), and writes an encrypted
   key file. Needs a real terminal (passphrase prompting requires a TTY).
2. Fund that wallet's address with SOL.
3. Copy `.env.example` to `.env` and fill in your free API keys (Alchemy,
   Shyft — see the comments in that file for where to get them).
4. Set `mode = "live"` in `config/config.toml` (or pass `--mode live`).
5. **Read the [honesty notes](#honesty-notes--known-limitations) below
   first** — several pieces of live trading need one more step from you
   before they're safe to trust with real funds.
6. `cargo run --bin trading-bot -- run --price-source live`

## Configuration

Two files, split deliberately:

- **`.env`** (gitignored; copy from `.env.example`) — secrets: RPC URLs,
  API keys, the encrypted wallet path, optional Telegram credentials.
- **`config/config.toml`** (committed) — everything else: which strategy
  runs, its parameters, risk limits (including per-trust-tier overrides),
  discovery/safety thresholds, execution slippage, and backtest settings.

See the comments in `config/config.toml` for every field. The trust-tier
system (`[risk.tiers.*]`) is the mechanism that makes the bot automatically
size positions smaller and enforce tighter stop-losses for tokens it knows
less about (still on pump.fun's bonding curve, or just-graduated) versus
ones with an established track record.

## Testing / verification

```bash
cargo test --workspace          # 121 tests, every crate
cargo clippy --workspace --all-targets   # zero warnings
cargo run --bin backtest -- --config config/config.toml --json
cargo run --bin trading-bot -- run --price-source mock --no-tui
```

Try varying `threshold_pct` (momentum) or `grid_step_pct` (grid) in
`config.toml` between backtest runs — the report numbers visibly change,
which is the acceptance check that they're computed from your parameters,
not hardcoded.

## Honesty notes / known limitations

This section exists so you know exactly what you're trusting before you
point real funds at this. Everything below is a deliberate, documented
scope decision made during a single build session — not a bug report.

### What's real and live-verified this session
- **Jupiter Swap API** (`lite-api.jup.ag`) — confirmed live and keyless;
  the mock/dry-run pipeline makes real `/quote` calls.
- **Jito Block Engine** — confirmed live (`getTipAccounts` returned real
  tip account addresses); bundle *submission* needs real funds to exercise
  further, matching the project's own $0-except-gas cost model.
- **Yellowstone gRPC** (`market_data`, `discovery`) — built and compiled
  against the real `yellowstone-grpc-client`/`-proto` crates, decode logic
  unit-tested against constructed real-shaped fixtures. Live streaming
  needs your own free Shyft `x-token`.
- **Solana RPC calls** (`safety::fetcher`) — real `get_account`/
  `get_token_largest_accounts` calls against whatever RPC you configure.

### The rule-based safety scorer is v1, not ML
You asked for the bot to eventually "learn and trade." A genuine ML
rug/quality model needs training data and infrastructure this build can't
honestly produce in one session. `safety::RuleBasedScorer` is real and
deterministic (mint/freeze authority, LP burn, holder concentration,
liquidity, age) and sits behind the `TokenSafetyScorer` trait — the exact
seam a learned scorer would plug into later without any pipeline redesign.
Rule-based-only for now; documented explicitly rather than overstated.

### `--price-source live` is a stub, not a full live pipeline (the most
important gap to know about)
`discovery`, `safety`, and `market_data`'s live Yellowstone client are all
real, independently unit-tested crates. What's **not yet wired together**
in `bin/trading-bot` is the glue that would: spawn the discovery watcher,
run newly-discovered tokens through the safety scorer, and dynamically add
whatever passes to the live price-streaming watchlist. Today,
`--price-source live` logs a warning and the bot idles waiting for Ctrl-C —
it does not automatically discover and trade new tokens end-to-end. Wiring
this is the natural next step and doesn't require redesigning anything
below `bin/trading-bot/src/main.rs`; it just wasn't completed this session,
and shipping it untested (no funded RPC key was available in this sandbox
to verify it against) would have been worse than being upfront about the
gap. If you extend this, `discovery::Discovery` + `safety::RuleBasedScorer`
+ `execution::Executor`'s existing phase-based routing are the pieces to
connect.

### pump.fun bonding-curve trading needs one more verification step before `mode = "live"`
`execution::pumpfun` has confirmed program ID, buy/sell instruction
discriminators, and constant-product pricing math (all verified this
session). The **full ordered account list** for the buy/sell instruction is
assembled from public documentation and community references, not from a
live transaction fetched in this sandbox (no funded RPC key was available
here). **Before enabling live bonding-curve trading**, cross-check
`PumpFunAccounts` in `crates/execution/src/pumpfun.rs` against a handful of
recent real mainnet `buy`/`sell` transactions (`getTransaction` over your
own RPC). Until then, live execution on a bonding-curve token is explicitly
refused in code (`Executor::execute_bonding_curve` returns an error in live
mode) rather than attempting an unverified instruction with real funds.
Migrated-token trading via Jupiter has no such caveat.

### Generic (non-pump.fun) new-pool discovery isn't implemented
`discovery` watches pump.fun's `create` and graduation events — the
overwhelming majority of new Solana memecoin launches. A token that skips
pump.fun entirely and launches straight on Raydium/Orca isn't auto-
discovered. It can still be traded via the (not-yet-wired, see above) live
watchlist or a manually configured entry.

### The backtester models fees/slippage, not order-book depth
`SimulatedExecutor` applies a configurable slippage % + fee bps + Jito tip
to every fill. It does not model real-time route price impact or
order-book depth the way a live Jupiter quote does. Good enough to compare
strategies against each other on the same data; not a claim of full market
realism.

### The TUI needs a real terminal
`crossterm`'s raw-mode terminal and `rpassword`'s passphrase prompt both
require a real TTY — neither works in a non-interactive/CI environment.
The dashboard's rendering logic is verified with `ratatui::backend::TestBackend`
(real assertions on rendered buffer content, no TTY needed) and the full
pipeline is verified headless via `--no-tui`; the visual dashboard itself
needs you to run it in your own terminal.

### Telegram alerts are optional and untested live
`bin/trading-bot/src/telegram.rs` is a real Bot API client; message
formatting is unit-tested. It only activates if you set
`TELEGRAM_BOT_TOKEN`/`TELEGRAM_CHAT_ID` in `.env`. Actually delivering a
message needs your own bot token, which this sandbox doesn't have.

### Wallet key zeroization caveat
Decrypted key buffers are wrapped in `Zeroizing`/explicitly zeroized
immediately after constructing the `solana_sdk::signature::Keypair`. That
`Keypair`'s own internal storage isn't itself guaranteed zeroize-aware —
full guaranteed zeroization through third-party internals can't be claimed,
only of the intermediate buffers this code directly controls.

## Safety

This bot can lose money, especially trading brand-new, thinly-traded, or
scam tokens — that's the nature of what you asked it to do. Start with
`--price-source mock` and `mode = "dry_run"`. Only fund the wallet with
capital you can afford to lose. The daily-loss circuit breaker and
per-trust-tier position limits are safety nets, not guarantees.
