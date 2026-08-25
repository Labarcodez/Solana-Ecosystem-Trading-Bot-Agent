-- Applied with CREATE TABLE IF NOT EXISTS at startup. No migration
-- framework for v1 - see README for why that's an acceptable simplification
-- at this stage (single-user, local-only, schema not yet stable enough to
-- warrant one).

CREATE TABLE IF NOT EXISTS trades (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    ts                  INTEGER NOT NULL,
    pair                TEXT NOT NULL,
    market_type         TEXT NOT NULL CHECK(market_type IN ('spot', 'margin', 'futures')),
    side                TEXT NOT NULL CHECK(side IN ('buy', 'sell')),
    qty                 REAL NOT NULL,
    price               REAL NOT NULL,
    quote_amount        REAL NOT NULL,
    fee_quote           REAL NOT NULL DEFAULT 0,
    funding_paid_quote  REAL NOT NULL DEFAULT 0,
    slippage_bps        INTEGER,
    strategy            TEXT NOT NULL,
    reason              TEXT NOT NULL,
    order_id            TEXT,
    status              TEXT NOT NULL CHECK(status IN ('pending', 'landed', 'failed', 'dry_run')),
    dry_run             INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_trades_pair ON trades(pair);
CREATE INDEX IF NOT EXISTS idx_trades_ts ON trades(ts);

CREATE TABLE IF NOT EXISTS positions (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    pair                TEXT NOT NULL,
    market_type         TEXT NOT NULL CHECK(market_type IN ('spot', 'margin', 'futures')),
    opened_ts           INTEGER NOT NULL,
    closed_ts           INTEGER,
    entry_price         REAL NOT NULL,
    exit_price          REAL,
    qty                 REAL NOT NULL,
    leverage            REAL NOT NULL DEFAULT 1,
    liquidation_price   REAL,
    strategy            TEXT NOT NULL,
    realized_pnl_quote  REAL,
    status              TEXT NOT NULL CHECK(status IN ('open', 'closed'))
);
CREATE INDEX IF NOT EXISTS idx_positions_status ON positions(status);
CREATE INDEX IF NOT EXISTS idx_positions_pair ON positions(pair);

CREATE TABLE IF NOT EXISTS equity_curve (
    id                      INTEGER PRIMARY KEY AUTOINCREMENT,
    ts                      INTEGER NOT NULL,
    equity_quote            REAL NOT NULL,
    realized_pnl_quote      REAL NOT NULL,
    unrealized_pnl_quote    REAL NOT NULL,
    daily_pnl_quote         REAL NOT NULL,
    daily_funding_quote     REAL NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_equity_curve_ts ON equity_curve(ts);

CREATE TABLE IF NOT EXISTS events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts          INTEGER NOT NULL,
    level       TEXT NOT NULL CHECK(level IN ('info', 'warn', 'error')),
    kind        TEXT NOT NULL,
    message     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts);
