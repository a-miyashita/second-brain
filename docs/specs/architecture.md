# Architecture

Related ADRs: 0001, 0002, 0004, 0005, 0008, 0014.

## Overview

```text
              ┌──────────────── second-brain (single binary, alias `sb`) ────────────────┐
 scheduler ──▶│ sb-cli: sync / ingest / search / show / doctor / setup / auth / mcp ...  │◀── agents
 (cron,       │                                                                          │    (skill → CLI --json,
  launchd,    │  sb-mcp ─────────────┐                                                   │     MCP → sb mcp)
  Task Sched.)│                      ▼                                                   │
              │  sb-pipeline: fetch → store raw → normalize → upsert → summarize → index │
              │      │             │                          │            │             │
              │      ▼             ▼                          ▼            ▼             │
              │  sources      sb-store (catalog,         sb-llm        SearchBackend     │
              │  sb-slack      raw files, secrets,      (summarizers)  (sqlite-fts;      │
              │  sb-google     settings, runs, issues)                  sqlite-vec later)│
              │  sb-extract                                                              │
              │      ▲ all traits and domain types live in sb-kernel                       │
              └──────┼───────────────────────────────────────────────────────────────────┘
                     │ HTTPS (rustls)
          Slack Web API / Google APIs / LLM APIs / local LLM server / LLM CLIs (subprocess)
```

## Workspace layout

```text
second-brain/
├── Cargo.toml                # [workspace]
├── crates/
│   ├── sb-kernel/            # domain types + traits; no I/O
│   ├── sb-store/             # SQLite catalog, migrations, raw file store,
│   │                         # secrets, settings, runs/issues, sqlite-fts backend
│   ├── sb-pipeline/          # ingestion pipeline, summarization queue, locking
│   ├── sb-llm/               # Summarizer impls (Anthropic, OpenAI(-compatible),
│   │                         # Google, Claude CLI, Copilot CLI), prompts
│   ├── sb-google/            # OAuth (PKCE loopback), Drive/Calendar/Meet clients,
│   │                         # google.meet and google.doc sources
│   ├── sb-slack/             # Slack client and slack.thread / slack.day sources
│   ├── sb-extract/           # bytes → Markdown for text/csv/html/docx/pptx/xlsx/pdf
│   │                         # (pure, Rust-only, feature-gated; see extract.md)
│   ├── sb-ondemand/          # on-demand sources without sync: web.page, local.file
│   ├── sb-mcp/               # MCP server (rmcp), phase 2
│   ├── sb-setup/             # home init, scheduler registration, skill install,
│   │                         # MCP client config, env setup
│   └── sb-cli/               # clap CLI; bins `second-brain` and `sb`
├── assets/
│   └── slack-app-manifest.yaml
├── docs/ (adrs/, specs/)
└── tests/                    # workspace-level integration tests
```

The skill files (`SKILL.md` and references, embedded with `include_str!`) are in
`crates/sb-setup/assets/skills/second-brain/`, because a published crate contains only
its own directory. The package names are `second-brain` for `sb-cli` and
`second-brain-<role>` for the others (ADR-0018, [release.md](release.md)).

Crate dependency rules:

- `sb-kernel` depends on no other workspace crate. Source, LLM and store crates depend
  only on `sb-kernel`, plus `sb-extract` where needed (`sb-google` for `google.doc`,
  `sb-ondemand`).
- An account may serve several source kinds (a Google account has `google.meet` and
  `google.doc`). Its adapter (`GoogleSource`) dispatches by source kind and delegates
  `sync` to the Meet source; `sb ingest` calls `fetch` and `normalize` with the kind.
  The `Source` trait and `SourceFactory` stay one adapter per account.
- `sb-pipeline` wires everything together. `sb-cli` and `sb-mcp` depend on
  `sb-pipeline` and `sb-store`.
- Only `sb-cli` (and `sb-mcp` for its server loop) may print to stdout.

## Core types (sb-kernel)

| Type | Meaning |
|---|---|
| `AccountId`, `AccountKind` | Account slug; `google`, `slack`, `imap`, `local`, `web` |
| `SourceKind` | `slack.thread`, `slack.day`, `google.meet`, `google.doc`, `mail.message`, `mail.thread`, `web.page`, `local.file` |
| `SourceRef` | Natural key `(account_id, source_kind, source_id)` plus `source_url` and timestamps |
| `RawBundle`, `RawObject` | Raw content with a role and media type |
| `Normalized` | Title, timestamps, metadata JSON, extracted sections, `SummaryInput`, optional native summary |
| `SectionKind` | `overview`, `decisions`, `action_items`, `details`, `background` |
| `SectionOrigin` | `generated`, `extracted`, `user` |
| `Generator` | `generator_kind`, `provider`, `model`, `prompt_version` |
| Traits | `Source`, `Summarizer`, `SearchBackend`, `Notifier`, `Clock` (for testing) |

## Runtime conventions

- Async runtime: `tokio`. HTTP client: `reqwest` with `rustls`. CLI parsing: `clap`
  (derive). Errors: `thiserror` in library crates, `anyhow` in `sb-cli`. Logging:
  `tracing`, filtered by `SB_LOG`.
- Logging destinations:
  - interactive runs log to stderr;
  - scheduled runs (`sb sync`) also log to `$SECOND_BRAIN_HOME/logs/<date>.log`,
    keeping 30 days.
- Concurrency:
  - fetching is per account with a bounded number of parallel requests per source,
    respecting source rate limits;
  - summarization uses a bounded pool per profile.
- One SQLite writer: a single connection owned by a store actor, or a mutex-guarded
  connection. Readers use separate connections, since WAL mode allows concurrent
  reads.
- Resumability (ADR-0012):
  - fetch workers hand results to the writer, which commits them in chunks
    (`pipeline.commit_batch`);
  - summaries are committed one by one;
  - cursors and `sync_queue` change only in the same transaction as the entries
    they cover.
- Cancellation: a `CancellationToken` (tokio-util) is passed through the whole
  pipeline. Signal handlers trigger it (first signal), or abort (second signal).
  Workers check it before starting each unit of work.

## `SECOND_BRAIN_HOME` layout

```text
$SECOND_BRAIN_HOME/          (0700)
├── second-brain.db          (0600) catalog + FTS index (+ -wal, -shm)
├── raw/<account_id>/<source_kind>/<yyyy>/<mm>/<source_id_slug>/<role>.<seq>.<ext>
├── logs/
├── locks/sync.lock
└── tmp/                     scratch (e.g. empty working dir for LLM CLIs)
```

`source_id_slug` is the source ID made filesystem-safe. When the source ID is long
or contains unsafe characters, a readable prefix is joined with a short hash.
