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
warnings. **152 tests, all passing.** The full pipeline has been run
end-to-end in `--mode dry_run --price-source mock`, including a real
momentum-strategy buy-then-sell round trip against **live** Jupiter `/quote`
calls (no fake data, no mocked HTTP responses at the integration level).
`--price-source live` genuinely discovers new pump.fun tokens, safety-scores
them, and trades whatever passes — see [Honesty
notes](#honesty-notes--known-limitations) for exactly what's verified end-to-
end versus what's real-but-unexercised (this sandbox never had a funded RPC
key or real SOL to run it against).

Read [Honesty notes](#honesty-notes--known-limitations) before you point
this at real funds — it explains precisely what's fully wired for live
trading today versus what's real-but-not-yet-connected.

## Architecture

```
discovery::pumpfun_watcher (Yellowstone) ──WatcherEvent──▶ live_pipeline coordinator
  (create / curve price update / graduation)      (bin/trading-bot)  │
                                                                      ├─▶ safety::RuleBasedScorer, per new token
                                                                      │     passed → TradableTokens map + TokenDiscovered
                                                                      │     failed → TokenRejectedBySafety
                                                                      └─▶ PriceTick (broadcast) — only forwarded for
                                                                          mints already in TradableTokens

PriceTick (broadcast) ──┬─▶ strategy_engine
                         ├─▶ risk_manager (SL/TP fires every tick, not signal-gated)
                         ├─▶ tui
                         └─▶ storage
strategy_engine ──Signal (mpsc)──▶ risk_manager (single veto point before execution;
                                                   looks up the signal's mint in TradableTokens)
risk_manager    ──ApprovedOrder──▶ executor (routes by TokenMeta.phase:
                                              bonding_curve → pump.fun client
                                              migrated       → Jupiter + Jito)
executor        ──AppEvent (broadcast)──┬─▶ storage
  (Fill / Rejected / CircuitBreaker)    ├─▶ tui
                                        ├─▶ risk_manager (feedback: PnL/positions)
                                        └─▶ telegram (optional)
```

A newly-seen mint only ever becomes tradable after `safety` clears it —
nothing downstream can bypass that gate: the coordinator only forwards
price ticks (and therefore only gives `strategy_engine` anything to react
to) for mints already in `TradableTokens`. `risk` is the single point
between a strategy's signal and the executor: no code path reaches
`execution` without going through it first. In `--price-source mock`, the
same `TradableTokens` map is seeded with one hardcoded asset instead of
being populated by discovery — everything downstream is identical either
way. `--mode dry_run` (the default) prevents any of this from ever signing
or submitting a real transaction, regardless of price source.

### Workspace layout

| Crate | Responsibility |
|---|---|
| `crates/core` (`bot-core`) | Domain types (`PriceTick`, `Signal`, `Fill`, `TokenMeta`, `TrustTier`, ...) + the `Strategy` trait. Zero I/O dependencies. |
| `crates/strategies` | `MomentumStrategy` (SMA crossover) and `GridStrategy`, plus the `build_strategy()` factory both binaries use — this is what guarantees live and backtest runs execute identical strategy logic. |
| `crates/risk` | `RiskManager`: per-trust-tier position sizing, stop-loss/take-profit, the daily-loss circuit breaker. |
| `crates/wallet` | Argon2id + AES-256-GCM encrypted keypair, interactive passphrase prompts. |
| `crates/market_data` | Real Yellowstone gRPC price streaming (generic vault-ratio pricing, works across any constant-product AMM) + a PumpSwap `Pool`-account decoder + a CSV mock-replay source. |
| `crates/discovery` | Real-time pump.fun `create`/graduation watcher via Yellowstone gRPC — also emits a live price tick on every bonding-curve balance change, not just at graduation. |
| `crates/safety` | `TokenSafetyScorer` trait + `RuleBasedScorer` (mint/freeze authority, LP burn, holder concentration, liquidity, age). |
| `crates/execution` | Jupiter (quote/swap), Jito (bundle landing), and a direct pump.fun bonding-curve client (IDL- and live-transaction-verified), routed by token phase. |
| `crates/storage` | SQLite trade/position/equity/event log, on its own dedicated thread. |
| `crates/tui` | The Ratatui dashboard. |
| `crates/backtester` | Replays historical data through the *same* `Strategy`/`RiskManager` code the live bot uses. |
| `bin/trading-bot` | The live binary — wires everything above together, including `live_pipeline.rs`'s discovery → safety → dynamic watchlist → execution glue for `--price-source live`. |
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
cargo test --workspace          # 152 tests, every crate
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
- **Solana RPC calls** (`safety::fetcher`, `live_pipeline`, `execution`) —
  real `get_account`/`get_token_largest_accounts`/`get_latest_blockhash`
  calls against whatever RPC you configure, for safety scoring, resolving
  a discovered token's owning SPL program, fetching pump.fun's
  `fee_recipient`, and building live transactions.
- **pump.fun bonding-curve program** (`execution::pumpfun`) — account list
  and PDA derivations verified against the official Anchor IDL *and* two
  real mainnet `buy`/`sell` transactions (see the dedicated section below
  for exactly what was and wasn't cross-checked).

### The rule-based safety scorer is v1, not ML
You asked for the bot to eventually "learn and trade." A genuine ML
rug/quality model needs training data and infrastructure this build can't
honestly produce in one session. `safety::RuleBasedScorer` is real and
deterministic (mint/freeze authority, LP burn, holder concentration,
liquidity, age) and sits behind the `TokenSafetyScorer` trait — the exact
seam a learned scorer would plug into later without any pipeline redesign.
Rule-based-only for now; documented explicitly rather than overstated.

### `--price-source live` is now wired end-to-end — with real, named gaps
`bin/trading-bot/src/live_pipeline.rs` spawns `discovery::pumpfun_watcher`,
runs every newly-created pump.fun token through `safety::RuleBasedScorer`
against real on-chain state, and adds whatever passes to a shared,
dynamically-growing tradable-token map that `strategy`/`risk`/`execution`
read from exactly the way they read the single hardcoded mock-mode asset.
A token that fails safety emits `TokenRejectedBySafety` and is never added.
This closes what used to be the biggest gap in this document. What's still
real-but-unexercised, and what's still genuinely missing:
- **Never run against mainnet with real funds or a live gRPC connection.**
  No funded RPC key or Shyft `x-token` was available in this sandbox. The
  wiring is verified as far as it can be here: it refuses cleanly and
  specifically (missing-env-var errors naming exactly which var) when
  `SHYFT_GRPC_ENDPOINT`/`SHYFT_X_TOKEN`/`ALCHEMY_RPC_URL` aren't set, and
  the full mock-mode pipeline (identical downstream code path) is verified
  live end-to-end.
- **A token's live price feed stops at graduation.** `TokenGraduated`
  still updates the token's `TokenPhase`/`TrustTier` in place, and the
  event is logged loudly (a `Warn`-level `AppEvent::Log`, not silence), but
  PumpSwap pool discovery for the newly-migrated pool isn't wired into the
  live price feed (see the PumpSwap section below) — so strategy signals
  and, more importantly, **stop-loss/take-profit monitoring for any open
  position in that token pause** until this is extended. Treat a graduation
  during an open live position as something to watch for manually today.
- **pump.fun's `fee_recipient` is resolved once at startup**, retried every
  30s until it succeeds, and then never refreshed again for the rest of the
  run. If pump.fun rotates it mid-run, live bonding-curve trades fail
  cleanly (a specific, logged error) rather than using a stale value — but
  they do fail until a restart.
- **Generic (non-pump.fun) new-pool discovery isn't implemented.**
  `discovery` watches pump.fun's `create` and graduation events — the
  overwhelming majority of new Solana memecoin launches. A token that skips
  pump.fun entirely and launches straight on Raydium/Orca isn't auto-
  discovered; it can still be traded via a manually configured entry.

### pump.fun bonding-curve trading is IDL- and live-transaction-verified
`execution::pumpfun`'s account list, PDA derivations, and instruction
discriminators were rebuilt from pump.fun's official Anchor IDL and then
cross-checked against two real, successful mainnet `buy`/`sell`
transactions fetched over public RPC — the decoded account list matched
the IDL account-for-account, and two of the derived global PDAs
(`event_authority`, `global_volume_accumulator`) match the exact addresses
observed on those real transactions (asserted in a regression test).
`Executor::execute_bonding_curve` now genuinely builds, signs, and submits
a live pump.fun trade rather than refusing outright. Residual, explicitly
documented uncertainty:
- The exact byte encoding of `buy`'s third argument (`track_volume`, an
  Anchor `OptionBool`) follows Anchor's standard `Option<T>` convention but
  wasn't independently decoded byte-for-byte from a real transaction's raw
  instruction data.
- `decode_global_fee_recipient`'s byte offset into the `Global` account is
  taken from the IDL's field order only, not cross-checked against a live
  account fetch (no RPC key was available in this sandbox).
- None of this has been exercised against mainnet with real funds — it's
  real, complete, unit-tested code that has not itself landed a live trade.

Migrated-token trading via Jupiter has none of these caveats — Jupiter's
`/quote`/`/swap` endpoints are used as documented, with a live `/quote`
call made even in dry-run.

### The PumpSwap pool decoder exists but isn't wired into live price discovery
`market_data::pumpswap_pool::decode_pool_account` decodes a PumpSwap `Pool`
account into its two vault addresses — everything `pool_price` needs to
watch a migrated token's live price. Its byte layout is taken from
PumpSwap's official IDL only; unlike pump.fun's bonding-curve accounts,
it was **not** independently cross-checked against a live transaction's raw
bytes this session (fetching one hit Solana's multi-address-lookup-table
resolution being ambiguous over the public RPC available here). It's also
not yet connected to anything that discovers *which* Pool account belongs
to a given migrated mint — that discovery step (subscribing to PumpSwap
pool-creation, matching by `base_mint`) is what a future pass would need to
add to close the "price feed stops at graduation" gap above.

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
