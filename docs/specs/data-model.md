# Data model (catalog schema)

Related ADRs: 0002, 0003, 0005, 0007, 0011.

SQLite, WAL mode, `foreign_keys = ON`. Timestamps are stored as RFC 3339 text in UTC.
IDs named `id INTEGER` are internal rowids. Entry IDs exposed to users are
`entry_uid`, a ULID string.

## Tables

### `schema_migrations`
`version INTEGER PRIMARY KEY, applied_at TEXT`

### `settings`
Key-value settings, with dotted keys and JSON values.

| Column | Type | Notes |
|---|---|---|
| `key` | TEXT PK | e.g. `summary.profile.default`, `search.backends`, `notify.sinks` |
| `value` | TEXT (JSON) | |
| `updated_at` | TEXT | |

Per-account settings live in `accounts.config`, not here.

### `accounts`

| Column | Type | Notes |
|---|---|---|
| `id` | TEXT PK | user-chosen slug, immutable |
| `kind` | TEXT | `google`, `slack`, `imap`, `local`, `web` |
| `label` | TEXT | display name |
| `identity` | TEXT | e.g. `alice@example.com`, `T0123:U0456` |
| `config` | TEXT (JSON) | source settings, e.g. Slack `full_channels` |
| `status` | TEXT | `active`, `disabled`, `needs_reauth` |
| `created_at`, `updated_at` | TEXT | |

The `local` and `web` pseudo-accounts are created by `sb setup home`.

### `secrets`

| Column | Type | Notes |
|---|---|---|
| `scope` | TEXT | `account:<id>` or `global` |
| `name` | TEXT | e.g. `google.refresh_token`, `google.oauth_client`, `slack.user_token`, `anthropic.api_key` |
| `value` | TEXT | never logged or printed unmasked |
| `expires_at` | TEXT NULL | |
| `updated_at` | TEXT | |

Primary key: `(scope, name)`.

### `entries`

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `entry_uid` | TEXT UNIQUE | ULID, public ID |
| `account_id` | TEXT FK | |
| `source_kind` | TEXT | |
| `source_id` | TEXT | source-native ID |
| `source_url` | TEXT NULL | canonical link to the original |
| `title` | TEXT | |
| `source_created_at` | TEXT NULL | when the original was created (meeting start, first message, doc created) |
| `source_updated_at` | TEXT NULL | last modification of the original (last reply, doc modified) |
| `ingested_at` | TEXT | first registration in second-brain (preserved on import) |
| `updated_at` | TEXT | last change of this row or its sections |
| `raw_status` | TEXT | `present`, `missing`, `fetch_failed` |
| `raw_hash` | TEXT NULL | hash over the raw bundle |
| `summary_status` | TEXT | `none` (source has no summary step), `pending`, `done`, `skipped` (below thresholds), `failed` |
| `summary_attempts` | INTEGER | failed attempts since the last success; entries at `summary.max_attempts` are not retried automatically |
| `summary_error` | TEXT NULL | last error message |
| `fetch_state` | TEXT (JSON) NULL | source-defined incremental state, e.g. `{"last_ts": "..."}` for Slack |
| `metadata` | TEXT (JSON) | source-specific; see per-source specs |
| `origin` | TEXT | `sync`, `ingest`, `import` |

Unique key: `(account_id, source_kind, source_id)`.
Indexes: `(source_kind, source_created_at)`, `(account_id)`, `(summary_status)`,
`(raw_status)`.

### `raw_objects`

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `entry_id` | FK → entries ON DELETE CASCADE | |
| `role` | TEXT | `primary`, `notes`, `transcript`, `attachment`, `extracted_text` |
| `seq` | INTEGER | segment number, 0-based. Appending sources add segments; a replace writes only 0 |
| `path` | TEXT | relative to `$SECOND_BRAIN_HOME` |
| `media_type` | TEXT | |
| `sha256` | TEXT | |
| `size` | INTEGER | |
| `fetched_at` | TEXT | |

Unique: `(entry_id, role, seq)`. The file path is deterministic
(`<role>.<seq>.<ext>`), so re-writing an uncommitted segment after a crash
overwrites the orphan. A full refetch deletes all segments and writes segment 0.
`raw_hash` in `entries` is the hash over all segments in order.

### `sections`

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | FTS content rowid |
| `entry_id` | FK ON DELETE CASCADE | |
| `kind` | TEXT | `overview`, `decisions`, `action_items`, `details`, `background` |
| `origin` | TEXT | `generated`, `extracted`, `user` |
| `position` | INTEGER | ordering |
| `text` | TEXT | Markdown |

At most one section per `(entry_id, kind)`. Re-summarization replaces only rows
with `origin = 'generated'`.

### `summaries`

At most one row per entry, overwritten on regeneration (ADR-0005).

| Column | Type | Notes |
|---|---|---|
| `entry_id` | PK, FK | |
| `generator_kind` | TEXT | `llm_api`, `llm_cli`, `local_llm`, `source_native` |
| `provider` | TEXT | `anthropic`, `openai`, `google`, `claude-cli`, `copilot-cli`, `foundry-local`, ... |
| `model` | TEXT | e.g. `claude-haiku-4-5`, `gemini-meet-notes` |
| `profile` | TEXT NULL | summarizer profile name used |
| `prompt_version` | TEXT NULL | e.g. `entry-summary/v1`; NULL for `source_native` |
| `input_hash` | TEXT | hash of the exact summarizer input |
| `generated_at` | TEXT | for imported entries, the original time if known |
| `usage` | TEXT (JSON) NULL | tokens in/out, duration |

### `sync_state`
Per-account, per-source cursors (Slack conversation `last_ts`, thread watch lists,
Calendar sync window, and so on).

`account_id, source_kind, key TEXT, value TEXT (JSON), updated_at` — primary key
`(account_id, source_kind, key)`.

### `sync_queue`
Discovered-but-unprocessed work, persisted in the same transaction as the cursor that
discovered it (ADR-0012).

| Column | Notes |
|---|---|
| `account_id`, `source_kind`, `source_id` | natural key of the item to fetch (PK) |
| `reason` | e.g. `new_thread`, `new_replies`, `watched`, `mention`, `calendar_attachment` |
| `hint` | JSON, e.g. `{"latest_reply": "..."}` to decide whether a fetch is needed |
| `enqueued_at`, `attempts`, `last_error` | |

A row is deleted in the transaction that commits the fetched entry.

### `cache`
Expiring cache, for example the Slack user directory.

`account_id, key, value (JSON), expires_at` — primary key `(account_id, key)`.

### `runs`

| Column | Notes |
|---|---|
| `id` | |
| `command` | `sync`, `sync --deep`, `ingest`, `resummarize`, `refetch`, `import`, ... |
| `trigger` | `manual`, `schedule`, `mcp` |
| `started_at`, `finished_at` | |
| `status` | `running`, `ok`, `partial`, `failed`, `interrupted`, `stopped_by_limit` |
| `stats` | JSON: per account/source counts of new, updated, summarized, failed; estimated LLM cost |
| `error` | |

### `issues`

| Column | Notes |
|---|---|
| `id` | |
| `code` | stable string, e.g. `auth.needs_reauth`, `sync.rate_limited`, `llm.failed`, `raw.fetch_failed` |
| `severity` | `info`, `warning`, `error` |
| `account_id` | nullable |
| `entry_id` | nullable |
| `message` | |
| `first_seen_at`, `last_seen_at`, `resolved_at` | |
| `notified_at` | |

Uniqueness: an open issue is unique by `(code, account_id, entry_id)`. Repeated
occurrences update `last_seen_at`.

### Search backend tables (`sqlite-fts`)

```sql
CREATE VIRTUAL TABLE fts_sections USING fts5(
  title, text, tokenize='trigram'
);  -- rowid = sections.id; title denormalized from entries
```

The table is a regular FTS5 table, which stores its own copy of the text. This is
the simplest option that keeps `snippet()` working: a contentless table cannot
produce snippets, and an external-content table would need the exact old values
for every delete. The text copy is small next to the trigram index. The table is
maintained by the backend, not by triggers: before an entry's sections change,
its rows are deleted by `rowid`, and afterwards they are inserted again. That
keeps backends swappable.

## Metadata JSON (common keys)

| Key | Used by |
|---|---|
| `participants`: `[{name, email?}]`, `absentees` | google.meet |
| `transcript_url`, `calendar_url`, `recurring` | google.meet |
| `channel_id`, `channel_name`, `channel_kind` (`channel`, `private`, `dm`, `group_dm`), `thread_ts`, `message_count` | slack.* |
| `mime_type`, `drive_file_id` | google.doc |
| `import_ref` | imported entries: the entry's identifier in the exporting tool |

## Invariants

1. `(account_id, source_kind, source_id)` identifies an entry forever. `entry_uid`
   never changes once assigned.
2. `raw_status = present` ⇔ at least one `raw_objects` row exists and all
   referenced files exist. Doctor checks this. Segment numbers of a role are
   contiguous from 0.
5. A `sync_state` cursor never points past an item that is neither committed as an
   entry nor present in `sync_queue`.
3. `summary_status = done` ⇒ a `summaries` row exists. The row is kept while an
   entry whose input changed waits for re-summarization (`pending`), so its old
   generated sections stay searchable and attributed until they are replaced.
4. A `source_native` summary never gets a `prompt_version`.
