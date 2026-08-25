# Kraken Exchange Trading Bot

A **local, terminal-based Kraken trading bot** that runs entirely on your own
machine. No cloud hosting, no third party ever touches your API keys. It's a
**multi-strategy framework**: a shared core (market data, execution, risk
management, dashboard) that pluggable strategies trade against — momentum,
grid, market-making, triangular arbitrage, and funding-rate carry, with more
you can write later.

This project used to trade the Solana ecosystem through Phantom-style wallet
signing. That approach didn't pan out, so it's been **completely rebuilt to
trade on Kraken instead** — a regulated centralized exchange with a real
order book, REST/WebSocket APIs, and (crucially) no wallet-signing surface to
fight with. Nothing Solana-specific survived the rewrite: no bonding curves,
no on-chain discovery, no rug-pull scoring. Kraken already vets what it
lists, so that entire safety layer had no equivalent problem to solve here.

The bot streams Kraken prices in real time (public WebSocket v2 for spot/
margin, polled REST tickers for futures), runs your chosen strategy against
that data, manages risk automatically (position sizing, stop-loss/take-
profit, a daily-loss circuit breaker, and — new for leveraged products — a
liquidation-distance floor), executes trades against Kraken's Spot, Margin,
and Futures APIs, logs every trade to a local SQLite database, and displays
it all in a live terminal dashboard. It also includes a backtesting engine
so you can test a strategy against real historical Kraken data before
risking anything.

Everything runs at **$0 cost** — Kraken's public market-data endpoints are
keyless and free. The only real costs are Kraken's own trading fees (see
[Fees](#fees-the-single-biggest-lever-a-bot-controls) below) and, if you
trade margin or futures, financing/funding costs — your own trading capital
being spent, not a service fee.

## Status: what actually works right now

Every crate compiles, is unit-tested, and passes clippy with zero warnings —
**124 tests, all passing** across the workspace. The full pipeline has been
run end-to-end in `--mode dry_run --price-source mock`: real historical
XBT/USD data replayed through the momentum strategy, a signal fired, the
risk manager sized and approved an order, and the executor made a **live**
call to Kraken's public `Ticker` endpoint to get a real fill price — no fake
data, no mocked HTTP responses at the integration level. That run is also
what caught and fixed a real bug this session (see
[Honesty notes](#honesty-notes--known-limitations)): Kraken's classic REST
API rejects the slash-separated pair spelling its own WebSocket v2 API
requires, so the client now converts at its own boundary.

Read [Honesty notes](#honesty-notes--known-limitations) before you point
this at real funds — it explains precisely what's live-verified from this
sandbox versus what needs your own funded Kraken API keys to exercise
further.

## How this bot tries to make money on Kraken

This isn't "buy low sell high" hand-waving — every mechanism below is a
specific, concrete thing the code does, grounded in how Kraken's API and fee
structure actually work (see [Sources](#sources) for what was researched
before building this).

### Fees: the single biggest lever a bot controls
Kraken charges **maker vs. taker fees** — a base tier of 0.25% maker /
0.40% taker, improving with 30-day volume down to 0.00%/0.05% at high tiers.
A **maker** order (one that adds liquidity to the book — a limit order that
doesn't immediately cross the spread) is cheaper than a **taker** order (one
that removes liquidity — a market order, or a limit order that crosses
immediately) on every single trade, before any strategy edge is even
considered. Kraken also supports **post-only** orders, which are rejected
outright rather than silently converted to taker if they'd cross — a hard
guarantee of maker pricing.

`execution::Executor` encodes this as policy, not as an afterthought: every
strategy-driven entry/exit (`OrderReason::Strategy`) is placed as a
**post-only limit order joining the current best bid/ask** — a real maker
fill. Every *protective* exit (`StopLoss`, `TakeProfit`, `LiquidationGuard`)
is placed as a **market order** — because a stop-loss that fails to fill
because the market moved away from a resting limit order isn't a stop-loss.
`backtester::SimulatedExecutor` mirrors this exactly (maker fee + zero
slippage for strategy fills, taker fee + slippage for protective exits), so
backtest numbers reflect the same fee structure live trading will actually
pay.

### Market making (`strategies::market_maker`)
Quotes around the current price: buys when flat, then sells once
`target_spread_pct` of favorable movement is captured, both legs as
post-only makers. This is the most direct way to harvest Kraken's
maker-rebate-relative-to-taker economics on a liquid pair like `XBT/USD` —
it doesn't need to predict direction, just capture spread + rebate on
round trips, with a requote cooldown so it doesn't flip-flop on noise.

### Triangular arbitrage (`strategies::triangular_arbitrage`)
Compares a pair's actual price (e.g. `ETH/XBT`) against the rate implied by
two other pairs (`ETH/USD` / `XBT/USD`) and trades when the mispricing
exceeds `min_mispricing_pct`. **Honestly scoped**: this is a single-leg
relative-value signal on the third pair, not an atomic, simultaneous 3-leg
arbitrage execution — Kraken has no batch/atomic cross-pair order primitive
this bot uses, so there's real execution-lag risk between legs. Documented
in the module itself, not just here.

### Funding-rate carry (`strategies::funding_carry`)
Kraken Futures perpetuals pay/charge **funding** hourly between longs and
shorts (capped ±0.50%/hr) to keep the perpetual's price anchored to spot.
When funding is persistently positive beyond `min_funding_rate_pct`, longs
are being paid — this strategy goes long to collect it, and exits (never
shorts, see the long-only limitation below) when funding flips unfavorable.
**Honestly scoped**: a real funding-carry trade is normally *hedged*
(long spot + short perp, market-neutral, funding is the entire return).
This bot's long-only architecture (below) can't represent that hedge, so
what's implemented is a directional tilt informed by funding, not a
market-neutral carry trade — real but a materially smaller edge than the
textbook version. Documented in the module itself.

### Margin and futures: real leverage, real liquidation risk
`AddOrderRequest.leverage` turns a plain Spot order into a Margin order via
Kraken's own single-endpoint design (there's no separate margin API to
call). Kraken Futures is a genuinely separate product with its own host and
auth scheme (`crates/execution/src/kraken_futures.rs`). Leverage amplifies
both the strategies above and their downside, so `crates/risk` enforces,
per market type (`[risk.tiers.spot/margin/futures]`):
- a hard **leverage cap**,
- a mandatory **liquidation-distance floor** — an order is rejected if its
  approximate liquidation distance (`100 / leverage`, a conservative,
  explicitly-simplified model — see the honesty note below) is closer than
  the configured minimum,
- a `LiquidationGuard` exit that fires as a market order the moment price
  crosses the position's estimated liquidation price, checked every tick
  alongside stop-loss/take-profit.

None of this is a promise of profit — leverage on Kraken can and does
liquidate positions. The risk tiers exist to make that a bounded, sized risk
rather than an unbounded one.

## Architecture

```
config/config.toml [kraken] pairs/futures_pairs
        │
        ▼
static Pair→PairMeta map (built once at startup — no discovery pipeline)
        │
        ▼
market_data: Kraken WebSocket v2 (spot/margin ticker stream)
             + polled Kraken Futures REST tickers (funding rate)
        │
        ▼  PriceTick (broadcast)
        ├─▶ strategy_engine (momentum / grid / market_maker /
        │                    triangular_arbitrage / funding_carry)
        ├─▶ risk_manager (SL/TP/LiquidationGuard fire every tick)
        ├─▶ tui
        └─▶ storage
strategy_engine ──Signal (mpsc)──▶ risk_manager (single veto point: position
                                    sizing, leverage cap, liquidation-distance
                                    floor, daily-loss circuit breaker)
risk_manager    ──ApprovedOrder──▶ executor (routes by PairMeta.market_type:
                                    spot/margin → Kraken Spot REST,
                                    futures     → Kraken Futures REST;
                                    maker post-only for strategy orders,
                                    taker market for protective exits)
executor        ──AppEvent (broadcast)──┬─▶ storage
  (Fill / Rejected / CircuitBreaker)    ├─▶ tui
                                        ├─▶ risk_manager (feedback: PnL/funding)
                                        └─▶ telegram (optional)
```

`risk` is the single point between a strategy's signal and the executor: no
code path reaches `execution` without going through it first. In
`--price-source mock`, the same static pair map is seeded from
`[kraken].pairs`/config defaults instead of a live WebSocket feed —
everything downstream is identical either way. `--mode dry_run` (the
default) makes real, live Kraken **public** API calls but never signs or
submits a private order, regardless of price source.

### Workspace layout

| Crate | Responsibility |
|---|---|
| `crates/core` (`bot-core`) | Domain types (`Pair`, `PriceTick`, `Signal`, `Fill`, `MarketType`, `RiskTier`, ...) + the `Strategy` trait. Zero I/O dependencies. |
| `crates/strategies` | `MomentumStrategy`, `GridStrategy`, `MarketMakerStrategy`, `TriangularArbitrageStrategy`, `FundingCarryStrategy`, plus the `build_strategy()` factory both binaries use — this is what guarantees live and backtest runs execute identical strategy logic. |
| `crates/risk` | `RiskManager`: per-risk-tier position sizing, stop-loss/take-profit, leverage caps, liquidation-distance floor + guard, the daily-loss circuit breaker (now funding-aware). |
| `crates/credentials` | Argon2id + AES-256-GCM encrypted Kraken API key/secret pair, interactive passphrase prompts. |
| `crates/market_data` | Kraken WebSocket v2 public ticker client + a CSV mock-replay source (real Kraken OHLC history). |
| `crates/execution` | Hand-rolled Kraken Spot REST client (public `Ticker`, private `AddOrder`/`CancelOrder`/`Balance`, HMAC-SHA512 signing) and Kraken Futures REST client (separate host/auth), routed by `Executor` per `MarketType`, with the maker/taker order-type policy described above. |
| `crates/storage` | SQLite trade/position/equity/event log, on its own dedicated thread. |
| `crates/tui` | The Ratatui dashboard. |
| `crates/backtester` | Replays historical data through the *same* `Strategy`/`RiskManager` code the live bot uses, including margin carrying-cost modeling. |
| `bin/trading-bot` | The live binary — wires everything above together. |
| `bin/backtest` | Thin CLI around `crates/backtester`. |

## Quick start

Requires a stable Rust toolchain (see `rust-toolchain.toml`).

```bash
# 1. Build everything
cargo build --workspace

# 2. Run the test suite
cargo test --workspace

# 3. Backtest a strategy against the bundled real historical Kraken data
cargo run --bin backtest -- --config config/config.toml

# 4. Run the full live-shaped pipeline with zero API keys and zero cost:
#    replays real historical XBT/USD data (from Kraken's own public OHLC
#    endpoint), runs your strategy, sizes and "fills" trades via a REAL
#    Kraken /Ticker call, but never signs or submits a private order.
cargo run --bin trading-bot -- run --price-source mock --no-tui
# drop --no-tui to see the live dashboard (needs a real terminal)
```

### Going live

1. On [kraken.com](https://kraken.com) → Settings → API, create a new key
   with **only**: Query Funds, Query Open & Closed Orders, Create & Modify
   Orders.

   **Never grant "Withdraw Funds" to a key this bot holds.** A bot that can
   place and cancel orders can be limited to trading your own account's
   capital around; a bot that can also withdraw can move that capital
   somewhere else entirely if the key or the machine holding it is ever
   compromised. This is the single most important operational-security
   decision in this whole setup.

   If you plan to trade Kraken Futures, create a **second**, separate key
   for that (Kraken's own recommendation) with the equivalent Futures
   permissions — never Withdraw there either.
2. `cargo run --bin trading-bot -- credentials init` — prompts for that API
   key/secret and a passphrase (twice, never echoed), and writes an
   encrypted file. Needs a real terminal. Add `--futures` to initialize the
   separate Futures credentials file.
3. Copy `.env.example` to `.env` — it just points at the encrypted
   credential file paths (no raw secrets ever go in `.env` or
   `config.toml`).
4. Set `mode = "live"` in `config/config.toml` (or pass `--mode live`), and
   configure `[kraken]` with the pairs you actually want to trade.
5. **Read the [honesty notes](#honesty-notes--known-limitations) below
   first** — it explains exactly what's live-verified from this sandbox
   versus what needs your own funded account to exercise further.
6. `cargo run --bin trading-bot -- run --price-source live`

## Configuration

Two files, split deliberately:

- **`.env`** (gitignored; copy from `.env.example`) — just the paths to
  your encrypted Kraken credential files, plus optional Telegram alert
  credentials. No raw API keys or secrets ever live in a file that could
  accidentally get committed.
- **`config/config.toml`** (committed) — everything else: which strategy
  runs and its parameters, risk limits (including per-risk-tier overrides
  for spot/margin/futures), the static tradable pair list, execution
  slippage, and backtest settings.

See the comments in `config/config.toml` for every field. The risk-tier
system (`[risk.tiers.*]`) is the mechanism that makes the bot automatically
size positions smaller, enforce tighter stop-losses, and demand a wider
liquidation cushion the more leverage a position carries — spot is the
loosest tier (no liquidation risk at all), futures the tightest.

### Why a static pair list instead of auto-discovering new listings
The Solana build watched pump.fun for brand-new, unvetted token launches —
that's what made a real-time discovery-and-safety-scoring pipeline
necessary. Kraken lists new assets rarely, after its own listing review, so
there's no equivalent firehose of unvetted new markets to watch for. This
build trades a **configured pair list** (`[kraken].pairs`/`futures_pairs`)
instead — simpler, and there was no real problem left for a discovery
watcher to solve here.

## Testing / verification

```bash
cargo test --workspace                     # 124 tests, every crate
cargo clippy --workspace --all-targets     # zero warnings
cargo run --bin backtest -- --config config/config.toml --json
cargo run --bin trading-bot -- run --price-source mock --no-tui
```

Try varying `threshold_pct` (momentum), `grid_step_pct` (grid), or
`target_spread_pct` (market maker) in `config.toml` between backtest runs —
the report numbers visibly change, which is the acceptance check that
they're computed from your parameters, not hardcoded.

## Honesty notes / known limitations

This section exists so you know exactly what you're trusting before you
point real funds at this. Everything below is a deliberate, documented scope
decision — not a bug report.

### What's real and live-verified this session
- **Kraken public REST** (`api.kraken.com/0/public/Ticker`, `/OHLC`) —
  confirmed live and keyless; the mock/dry-run pipeline makes a real
  `Ticker` call for every fill, and `data/sample_xbtusd.csv` is 721 hours
  of real XBT/USD closes pulled live from Kraken's own `OHLC` endpoint this
  session (not a fabricated or third-party sample).
- **Kraken public WebSocket v2** (`wss://ws.kraken.com/v2`) — confirmed
  live via a raw TLS handshake returning HTTP 101 this session; the ticker
  message parser is tested against Kraken's own documented example payload.
- **A real bug was caught by actually running the pipeline, not just unit
  tests**: Kraken's classic REST API rejects the slash-separated pair
  spelling (`"XBT/USD"` → `EQuery:Unknown asset pair`) that its own
  WebSocket v2 API requires (confirmed via direct `curl` against both).
  `crates/execution::kraken_spot::rest_pair_symbol` converts at the REST
  client's own boundary so the rest of the codebase can carry one spelling
  end-to-end. Re-verified after the fix: a full dry-run momentum
  buy-then-sell round trip now completes against the live `Ticker`
  endpoint with no error.
- **Request signing is independently cross-checked, not just
  self-consistent** — Kraken Spot's HMAC-SHA512 signing was checked against
  Kraken's own published worked example (secret/path/nonce/postdata from
  their docs) via an independent Python computation matching bit-for-bit.
  Kraken Futures' signing algorithm has **no official worked example
  published**, so it's only cross-checked for internal self-consistency
  (same inputs → same signature, different inputs → different signature) —
  a weaker verification tier, called out explicitly rather than implied to
  be equally strong.

### What's real but not exercised against a funded account
No funded Kraken API key was available in this sandbox. Everything above
covers what's genuinely live-verifiable without one. What's real,
complete, and unit-tested — but has not itself placed a live order:
- **`AddOrder`/`CancelOrder`/`Balance` (Spot and Margin)** — request
  building and signing match Kraken's documented format; response parsing
  is tested against realistic fixture JSON. `dry_run` builds and would-sign
  these requests but always stops short of sending them.
- **Kraken Futures `sendorder`/`cancelorder`** — same status: real,
  fixture-tested, unexercised against a live account.
- **Live balance lookup** (`fetch_live_balance_quote`) — tries a small,
  documented set of heuristic asset-code spellings (`USD`, `ZUSD`, `XUSD`)
  against Kraken's `Balance` response keys. This is **not** a complete
  Kraken asset-code table; an unusual base currency may need the heuristic
  extended.

### The long-only architecture can't represent a true hedge or a short
A `Signal` only ever opens via `Buy` and closes via `Sell` — there is no
short-selling representation anywhere in the pipeline. This is why
`funding_carry` is documented above as a directional tilt rather than a
true hedged carry trade, and why margin/futures in this build are
long-leverage-only, not short-capable. Extending to real shorts would touch
`core::types::Signal`, every strategy, and both `risk`/`execution` — a
larger change than this rebuild's scope.

### The liquidation-price model is a deliberate simplification
`risk::approx_liquidation_distance_pct(leverage) = 100.0 / leverage` is a
naive, symmetric approximation — **not** Kraken's actual maintenance-margin
schedule, which varies by asset, position size, and account-wide margin
usage. It's used as a conservative mandatory floor (reject if the
approximate distance is too tight), not as a precise prediction of Kraken's
real liquidation price. Treat the `LiquidationGuard` as a safety net with
margin for the model's own imprecision, not an exact trigger.

### The backtester models fees and financing, not order-book depth
`SimulatedExecutor` applies configurable maker/taker fees (mirroring the
executor's real maker/taker policy) plus a slippage % on taker fills, and
`engine::margin_carrying_cost` charges estimated daily interest on the
borrowed portion of a leveraged position. It does not model real-time
order-book depth or price impact the way a live Kraken order actually
would. Good enough to compare strategies against each other and get a
realistic fee/financing-adjusted picture; not a claim of full market
realism.

### The TUI needs a real terminal
`crossterm`'s raw-mode terminal and `rpassword`'s passphrase prompt both
require a real TTY — neither works in a non-interactive/CI environment. The
dashboard's rendering logic is verified with `ratatui::backend::TestBackend`
(real assertions on rendered buffer content, no TTY needed) and the full
pipeline is verified headless via `--no-tui`; the visual dashboard itself
needs you to run it in your own terminal.

### Telegram alerts are optional and untested live
`bin/trading-bot/src/telegram.rs` is a real Bot API client; message
formatting is unit-tested. It only activates if you set
`TELEGRAM_BOT_TOKEN`/`TELEGRAM_CHAT_ID` in `.env`. Actually delivering a
message needs your own bot token, which this sandbox doesn't have.

### Credentials zeroization
`KrakenCredentials` holds the API key and secret in `Zeroizing<String>`
end-to-end — this crate fully controls its own memory (a genuine
improvement over the old Solana build's `solana_sdk::Keypair`, whose
internal storage wasn't itself guaranteed zeroize-aware). The encrypted
file on disk uses the same Argon2id (OWASP interactive baseline) +
AES-256-GCM scheme as before; a wrong passphrase is rejected with a clear
error rather than silently producing garbage credentials.

## Safety

This bot can lose money — including all of it, faster than spot, if you
enable margin or futures leverage. Start with `--price-source mock` and
`mode = "dry_run"`. Only fund your Kraken account with capital you can
afford to lose. The daily-loss circuit breaker, per-risk-tier position
limits, and the liquidation-distance floor are safety nets, not guarantees.
**Never grant a bot-held API key "Withdraw Funds" permission** — see
[Going live](#going-live) above.

## Sources

Research consulted while designing the Kraken integration and the
"how to make money on Kraken" strategies above:

- [Kraken API | REST, WebSocket and FIX APIs](https://www.kraken.com/features/trading-api)
- [Spot REST Authentication | Kraken API Center](https://docs.kraken.com/api/docs/guides/spot-rest-auth/)
- [Trading | Kraken API Center](https://docs.kraken.com/api/docs/category/rest-api/trading/)
- [Fee Structures | Kraken](https://www.kraken.com/features/fee-schedule)
- [Ticker (Level 1) | Kraken API Center](https://docs.kraken.com/api/docs/websocket-v2/ticker/)
- [Candles (OHLC) | Kraken API Center](https://docs.kraken.com/api/docs/websocket-v2/ohlc/)
- [Spot REST Rate Limits | Kraken API Center](https://docs.kraken.com/api/docs/guides/spot-rest-ratelimits/)
- [Margin trading pairs and their maximum leverage | Kraken](https://support.kraken.com/articles/227876608-margin-trading-pairs-and-their-maximum-leverage)
- [Get Tradable Asset Pairs - Kraken Developers](https://docs.kraken.com/api-reference/market-data/get-tradable-asset-pairs)
- [A Quick Primer on Funding Rates - Kraken Blog](https://blog.kraken.com/product/quick-primer-on-funding-rates)
- [Send order | Kraken API Center](https://docs.kraken.com/api/docs/futures-api/trading/send-order/)
- [Cancel order - Kraken Futures API](https://docs.kraken.com/api/docs/futures-api/trading/cancel-order/)
- [API key permissions - Kraken Developers](https://docs.kraken.com/exchange/guides/rest/api-keys)
- [Order types & options | Kraken](https://support.kraken.com/sections/200577136-order-types)
