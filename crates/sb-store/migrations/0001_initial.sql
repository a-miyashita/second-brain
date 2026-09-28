-- Initial catalog schema (docs/specs/data-model.md).

CREATE TABLE settings (
  key        TEXT PRIMARY KEY,
  value      TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE accounts (
  id         TEXT PRIMARY KEY,
  kind       TEXT NOT NULL,
  label      TEXT NOT NULL,
  identity   TEXT,
  config     TEXT NOT NULL DEFAULT '{}',
  status     TEXT NOT NULL DEFAULT 'active',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE secrets (
  scope      TEXT NOT NULL,
  name       TEXT NOT NULL,
  value      TEXT NOT NULL,
  expires_at TEXT,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (scope, name)
);

CREATE TABLE entries (
  id                INTEGER PRIMARY KEY,
  entry_uid         TEXT NOT NULL UNIQUE,
  account_id        TEXT NOT NULL REFERENCES accounts(id),
  source_kind       TEXT NOT NULL,
  source_id         TEXT NOT NULL,
  source_url        TEXT,
  title             TEXT NOT NULL,
  source_created_at TEXT,
  source_updated_at TEXT,
  ingested_at       TEXT NOT NULL,
  updated_at        TEXT NOT NULL,
  raw_status        TEXT NOT NULL,
  raw_hash          TEXT,
  summary_status    TEXT NOT NULL,
  summary_attempts  INTEGER NOT NULL DEFAULT 0,
  summary_error     TEXT,
  fetch_state       TEXT,
  metadata          TEXT NOT NULL DEFAULT '{}',
  origin            TEXT NOT NULL,
  UNIQUE (account_id, source_kind, source_id)
);
CREATE INDEX entries_kind_created ON entries(source_kind, source_created_at);
CREATE INDEX entries_account ON entries(account_id);
CREATE INDEX entries_summary_status ON entries(summary_status);
CREATE INDEX entries_raw_status ON entries(raw_status);

CREATE TABLE raw_objects (
  id         INTEGER PRIMARY KEY,
  entry_id   INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
  role       TEXT NOT NULL,
  seq        INTEGER NOT NULL,
  path       TEXT NOT NULL,
  media_type TEXT NOT NULL,
  sha256     TEXT NOT NULL,
  size       INTEGER NOT NULL,
  fetched_at TEXT NOT NULL,
  UNIQUE (entry_id, role, seq)
);

CREATE TABLE sections (
  id       INTEGER PRIMARY KEY,
  entry_id INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
  kind     TEXT NOT NULL,
  origin   TEXT NOT NULL,
  position INTEGER NOT NULL,
  text     TEXT NOT NULL,
  UNIQUE (entry_id, kind)
);

CREATE TABLE summaries (
  entry_id       INTEGER PRIMARY KEY REFERENCES entries(id) ON DELETE CASCADE,
  generator_kind TEXT NOT NULL,
  provider       TEXT NOT NULL,
  model          TEXT NOT NULL,
  profile        TEXT,
  prompt_version TEXT,
  input_hash     TEXT NOT NULL,
  generated_at   TEXT NOT NULL,
  usage          TEXT
);

CREATE TABLE sync_state (
  account_id  TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  source_kind TEXT NOT NULL,
  key         TEXT NOT NULL,
  value       TEXT NOT NULL,
  updated_at  TEXT NOT NULL,
  PRIMARY KEY (account_id, source_kind, key)
);

CREATE TABLE sync_queue (
  account_id  TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  source_kind TEXT NOT NULL,
  source_id   TEXT NOT NULL,
  reason      TEXT NOT NULL,
  hint        TEXT NOT NULL DEFAULT '{}',
  enqueued_at TEXT NOT NULL,
  attempts    INTEGER NOT NULL DEFAULT 0,
  last_error  TEXT,
  PRIMARY KEY (account_id, source_kind, source_id)
);

CREATE TABLE cache (
  account_id TEXT NOT NULL,
  key        TEXT NOT NULL,
  value      TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  PRIMARY KEY (account_id, key)
);

CREATE TABLE runs (
  id          INTEGER PRIMARY KEY,
  command     TEXT NOT NULL,
  trigger     TEXT NOT NULL,
  started_at  TEXT NOT NULL,
  finished_at TEXT,
  status      TEXT NOT NULL,
  stats       TEXT NOT NULL DEFAULT '{}',
  error       TEXT
);
CREATE INDEX runs_started ON runs(started_at);

CREATE TABLE issues (
  id            INTEGER PRIMARY KEY,
  code          TEXT NOT NULL,
  severity      TEXT NOT NULL,
  account_id    TEXT,
  entry_id      INTEGER,
  message       TEXT NOT NULL,
  first_seen_at TEXT NOT NULL,
  last_seen_at  TEXT NOT NULL,
  resolved_at   TEXT,
  notified_at   TEXT
);
-- An open issue is unique by (code, account_id, entry_id).
CREATE UNIQUE INDEX issues_open_unique
  ON issues(code, IFNULL(account_id, ''), IFNULL(entry_id, 0))
  WHERE resolved_at IS NULL;

-- sqlite-fts search backend. One row per section; rowid = sections.id.
-- The title is denormalized from entries so title matches score.
CREATE VIRTUAL TABLE fts_sections USING fts5(
  title, text, tokenize = 'trigram'
);
