# ADR-0002: Three data layers — raw data is the source of truth

- Status: Accepted
- Date: 2026-09-29

## Context

A simple design keeps only normalized entries (for example, one Markdown file per
entry) and discards the raw material. For meetings that means dropping transcripts,
which are most of the data volume.

The requirements rule that out:

- Summaries must be **regenerable with a different model later**. For example, redo
  a `claude-haiku` summary with `claude-opus`, or replace Gemini's Meet notes with
  another model's output. That is impossible without the raw input.
- Some entries arrive **without raw data**. Entries imported from another tool
  that did not keep the raw material are the main case. They must still be
  searchable, and it must be possible to fetch their raw data later.

## Decision

Data is kept in three layers.

| Layer | Storage | Role | Rebuildable from |
|---|---|---|---|
| **Raw** | Files under `$SECOND_BRAIN_HOME/raw/` | Source of truth for content: exactly what was fetched from the source | The source, via `sb refetch` |
| **Catalog** | SQLite (`second-brain.db`) | Entries, metadata, sections (including summaries), generator info, accounts, settings, secrets, sync state | Sections: from raw (re-extract / re-summarize). Metadata, settings, accounts and secrets: **not rebuildable** |
| **Search index** | Owned by the search backend (FTS5 tables in the same DB for MVP) | Derived data for retrieval | From the catalog, via `sb index rebuild` |

Rules:

1. Every entry has a stable **natural key** `(account_id, source_kind, source_id)`.
   `source_id` is the source's own identifier, for example a Drive file ID or a
   Slack `channel_id:thread_ts`. Ingestion from any path (sync, single ingest,
   import) upserts by this key.
2. Every entry has a `raw_status`: `present`, `missing` or `fetch_failed`.
   - Entries with `missing` / `fetch_failed` are still indexed and searchable.
   - Operations that need raw data (re-extraction, re-summarization) skip them and
     report the reason.
   - `sb refetch` fetches raw data by natural key and moves them to `present`.
3. Agents read entries through the CLI (`sb show`) or MCP. There is **no**
   file-per-entry Markdown tree.
4. Raw files are never modified after being written. There are two cases:
   - A source that grows by appending (e.g. a Slack thread) adds a new
     **segment** containing only the new part.
   - A source that is rewritten (e.g. a Google Doc), or a full refetch, replaces
     all segments.

   There is no version history beyond that. See ADR-0005 and ADR-0012.

## Consequences

- Any summary can be regenerated as long as raw data is present.
- Importing data from other tools fits without special cases. Imported entries
  are simply `raw_status = missing` until refetched (see [specs/import-format.md](../specs/import-format.md)).
- The catalog DB is now valuable, because it holds accounts, secrets and
  settings. It is no longer disposable, so `sb doctor` warns when it has no
  backup. Backups are the user's responsibility; `sb backup` is a later option.
- Disk usage grows, since transcripts are kept. This is acceptable for
  personal-scale data (thousands to tens of thousands of entries).

## Alternatives considered

- **Keep Markdown files as the source of truth**: cannot hold raw data and
  generator metadata cleanly, and duplicates what the catalog holds.
- **Store raw blobs inside SQLite**: simpler backup, but bloats the DB and makes
  large transcripts awkward. Files plus a SHA-256 in the catalog are enough.
