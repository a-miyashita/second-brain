-- Summarization usage ledger and budget period snapshots (ADR-0013,
-- docs/specs/data-model.md).

-- Append-only: one row per paid summarization attempt. The spend of a period is
-- always summed from here.
CREATE TABLE llm_usage (
  id             INTEGER PRIMARY KEY,
  at             TEXT NOT NULL,
  run_id         INTEGER,
  entry_id       INTEGER,
  profile        TEXT NOT NULL,
  generator_kind TEXT NOT NULL,
  provider       TEXT NOT NULL,
  model          TEXT NOT NULL,
  input_tokens   INTEGER NOT NULL DEFAULT 0,
  output_tokens  INTEGER NOT NULL DEFAULT 0,
  calls          INTEGER NOT NULL DEFAULT 0,
  cost_usd       REAL,
  outcome        TEXT NOT NULL
);
CREATE INDEX llm_usage_at ON llm_usage(at);

-- One row per budget period the tool has evaluated: the budget side of the
-- history. Spend is not stored here.
CREATE TABLE budget_periods (
  kind           TEXT NOT NULL,
  period_start   TEXT NOT NULL,
  starts_at      TEXT NOT NULL,
  ends_at        TEXT NOT NULL,
  cap_usd        REAL,
  cap_updated_at TEXT NOT NULL,
  stopped_at     TEXT,
  PRIMARY KEY (kind, period_start)
);

-- Default caps, as visible settings. A value the user already set is kept.
INSERT OR IGNORE INTO settings(key, value, updated_at)
VALUES ('summary.budget.weekly_usd', '2.0', strftime('%Y-%m-%dT%H:%M:%SZ', 'now'));
INSERT OR IGNORE INTO settings(key, value, updated_at)
VALUES ('summary.budget.monthly_usd', '10.0', strftime('%Y-%m-%dT%H:%M:%SZ', 'now'));
