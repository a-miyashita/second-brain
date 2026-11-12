-- ADR-0022: document links found in Slack entries and their ingest state.
-- One row per (Slack account, link key); the first reference wins.
CREATE TABLE discovered_links (
  account_id      TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  link_key        TEXT NOT NULL,
  source_kind     TEXT NOT NULL,
  url             TEXT NOT NULL,
  host            TEXT NOT NULL,
  first_entry_id  INTEGER REFERENCES entries(id) ON DELETE SET NULL,
  first_seen_at   TEXT NOT NULL,
  excerpt         TEXT,
  status          TEXT NOT NULL DEFAULT 'pending',
  entry_id        INTEGER REFERENCES entries(id) ON DELETE SET NULL,
  attempts        INTEGER NOT NULL DEFAULT 0,
  next_attempt_at TEXT,
  last_error      TEXT,
  updated_at      TEXT NOT NULL,
  PRIMARY KEY (account_id, link_key)
);
CREATE INDEX discovered_links_status ON discovered_links(status, source_kind);
