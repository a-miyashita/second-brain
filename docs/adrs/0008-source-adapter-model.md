# ADR-0008: Source adapter model

- Status: Accepted
- Date: 2026-09-29

## Context

Sources to support, with the first two in the MVP:

1. Slack threads (and channel-day conversations)
2. Google Meet: Gemini notes, plus transcripts when available
3. Google Docs / Slides / Sheets and other Drive files
4. Email: Gmail API, and IMAP
5. Arbitrary URLs
6. Local files (docx, pptx, pdf, txt, md, csv, tsv, ...)

There are two ingestion styles:

- **incremental sync**: scan everything, fetch the difference;
- **single-item ingest**: "add this document", usually driven by an agent, with a
  user-supplied "why this matters" context.

Both styles must remain.

## Decision

A source adapter implements the `Source` trait in `sb-core`:

```text
kind() -> SourceKind                     // e.g. slack.thread, google.meet
account_kind() -> AccountKind            // google / slack / imap / local / web
resolve(locator) -> Option<SourceRef>    // URL or path -> natural key (single ingest)
sync(ctx, mode) -> stream of SourceRef   // incremental discovery (optional)
fetch(ctx, ref, fetch_state?) -> RawBundle  // raw objects + source metadata;
                                         // with a fetch_state, only the new part
                                         // (appended segment) when supported
normalize(ctx, raw) -> Normalized        // metadata, extracted sections,
                                         // summary input, optional native summary
```

- `SourceRef` is the natural key (`account_id`, `source_kind`, `source_id`) plus
  the canonical `source_url` and timestamps.
- `RawBundle` is one or more raw objects with roles such as `primary`,
  `transcript`, `notes` or `attachment`, plus source metadata and the new
  `fetch_state`. It is marked either `Replace` or `Append`: an append adds a
  segment (ADR-0012).
- `Normalized` is:
  - pure: raw in, structure out, with no network access;
  - deterministic: the same raw data always gives the same result;
  - therefore re-runnable at any time (`sb reextract`).
- `sync` is optional. Sources that only make sense on demand (URL, local file) do
  not implement it.
- The **ingestion pipeline** is shared by sync, single ingest, refetch and import:

  ```text
  fetch → store raw → normalize → upsert entry/sections
        → summarize if needed → index
  ```

  Every stage commits in small units, and cursors never run ahead of committed
  data, so any run can be interrupted and resumed (ADR-0012). `normalize` receives
  all segments of an entry.

- **Summarization is decoupled from fetching.** Sync writes entries immediately
  with `summary_status = pending`. Summarization runs as a separate pipeline stage,
  with limits (`--max-summaries`) and concurrency. An entry is re-summarized only
  if its `input_hash` changed.
- Text extraction for document formats (docx, pptx, pdf, html) lives in
  `sb-extract`, shared by Google Docs export, URL and local-file sources.
- Email is modelled as the generic kinds `mail.message` / `mail.thread`, with
  transport adapters for Gmail API and IMAP under the same kinds. IMAP accounts
  store server, port, TLS mode and username, with the password or app password as
  a secret. **Not implemented in the MVP**, but the kinds and account kind are
  reserved.
- Link discovery (`scan-links`) is a read-only query over the catalog. It
  never ingests automatically.

## Consequences

- New sources are added without touching the pipeline, search or CLI plumbing,
  apart from registering the adapter.
- Because `normalize` is pure, parser improvements can be applied to all stored
  entries with `sb reextract`, without hitting the network.
- Per-source specs live under `docs/specs/source-*.md`.
