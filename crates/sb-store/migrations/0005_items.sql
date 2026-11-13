-- ADR-0024: structured items extracted with a summary, and links between items.
-- `items` is generated data owned by the entry: replaced together with the summary.
CREATE TABLE items (
  id          INTEGER PRIMARY KEY,
  entry_id    INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
  ordinal     INTEGER NOT NULL,
  kind        TEXT NOT NULL,
  text        TEXT NOT NULL,
  status      TEXT,
  owner       TEXT,
  requester   TEXT,
  due         TEXT,
  actors      TEXT NOT NULL DEFAULT '[]',
  occurred_at TEXT,
  confidence  TEXT NOT NULL,
  evidence    TEXT,
  scope_key   TEXT NOT NULL,
  UNIQUE (entry_id, ordinal)
);
CREATE INDEX items_kind_status ON items(kind, status);
CREATE INDEX items_scope ON items(scope_key, occurred_at);
CREATE INDEX items_occurred ON items(occurred_at);

-- Derived and rebuildable. Empty until the reconcile stage exists.
CREATE TABLE item_links (
  id              INTEGER PRIMARY KEY,
  from_item       INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  to_item         INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  relation        TEXT NOT NULL,
  confidence      TEXT NOT NULL,
  evidence        TEXT,
  linked_by_model TEXT NOT NULL,
  prompt_version  TEXT NOT NULL,
  created_at      TEXT NOT NULL,
  UNIQUE (from_item, to_item, relation)
);
CREATE INDEX item_links_from ON item_links(from_item);
CREATE INDEX item_links_to ON item_links(to_item);
