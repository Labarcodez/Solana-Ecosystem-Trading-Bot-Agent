-- Applied with CREATE TABLE IF NOT EXISTS at startup. No migration
-- framework for v1 - see README for why that's an acceptable simplification
-- at this stage (single-user, local-only, schema not yet stable enough to
-- warrant one).

CREATE TABLE IF NOT EXISTS trades (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    ts              INTEGER NOT NULL,
    mint            TEXT NOT NULL,
    side            TEXT NOT NULL CHECK(side IN ('buy', 'sell')),
    qty             REAL NOT NULL,
    price           REAL NOT NULL,
    sol_amount      REAL NOT NULL,
    fee_sol         REAL NOT NULL DEFAULT 0,
    jito_tip_sol    REAL NOT NULL DEFAULT 0,
    slippage_bps    INTEGER,
    strategy        TEXT NOT NULL,
    reason          TEXT NOT NULL,
    tx_signature    TEXT,
    bundle_id       TEXT,
    status          TEXT NOT NULL CHECK(status IN ('pending', 'landed', 'failed', 'dry_run')),
    dry_run         INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_trades_mint ON trades(mint);
CREATE INDEX IF NOT EXISTS idx_trades_ts ON trades(ts);

CREATE TABLE IF NOT EXISTS positions (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    mint                TEXT NOT NULL,
    opened_ts           INTEGER NOT NULL,
    closed_ts           INTEGER,
    entry_price         REAL NOT NULL,
    exit_price          REAL,
    qty                 REAL NOT NULL,
    strategy            TEXT NOT NULL,
    realized_pnl_sol    REAL,
    status              TEXT NOT NULL CHECK(status IN ('open', 'closed'))
);
CREATE INDEX IF NOT EXISTS idx_positions_status ON positions(status);
CREATE INDEX IF NOT EXISTS idx_positions_mint ON positions(mint);

CREATE TABLE IF NOT EXISTS equity_curve (
    id                      INTEGER PRIMARY KEY AUTOINCREMENT,
    ts                      INTEGER NOT NULL,
    equity_sol              REAL NOT NULL,
    realized_pnl_sol        REAL NOT NULL,
    unrealized_pnl_sol      REAL NOT NULL,
    daily_pnl_sol           REAL NOT NULL
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
