# Design documents

## ADRs (`adrs/`)

ADRs record decisions. Once accepted, an ADR is **not rewritten or deleted**. To
correct outdated details, append an `## Amendments` section with a date. For a
substantially different decision, write a new ADR that supersedes the old one.

| # | Title |
|---|---|
| [0001](adrs/0001-rust-single-binary-workspace.md) | Rust workspace producing a single `second-brain` binary |
| [0002](adrs/0002-three-layer-data-raw-is-source-of-truth.md) | Three data layers — raw data is the source of truth |
| [0003](adrs/0003-sqlite-catalog-with-secrets-0600.md) | SQLite catalog, including secrets, protected by file permissions |
| [0004](adrs/0004-pluggable-search-backend.md) | Pluggable search backend; FTS5 trigram first, vectors later |
| [0005](adrs/0005-summarizer-abstraction-and-generator-provenance.md) | Summarizer abstraction and generator provenance |
| [0006](adrs/0006-native-google-oauth.md) | Native Google OAuth and REST clients (no rclone) |
| [0007](adrs/0007-multi-account.md) | Multiple accounts for every source |
| [0008](adrs/0008-source-adapter-model.md) | Source adapter model |
| [0009](adrs/0009-installation-layout-scheduling-distribution.md) | Installation layout, scheduling and distribution |
| [0010](adrs/0010-agent-integration-skill-and-mcp.md) | Agent integration through a global skill and an MCP server |
| [0011](adrs/0011-notifications.md) | Notifications — pluggable sinks, `doctor` as the baseline |
| [0012](adrs/0012-resumable-incremental-ingestion.md) | Resumable, interruptible and incremental ingestion |
| [0013](adrs/0013-summarization-budget-caps.md) | Summarization budget caps (weekly and monthly, from a usage ledger) |
| [0014](adrs/0014-single-item-ingest.md) | Single-item ingest for Google Docs, web pages and local files |
| [0015](adrs/0015-local-llm-not-recommended.md) | Local LLMs are not recommended for summaries; the Foundry Local wizard is dropped |

## Specs (`specs/`)

Specs describe how things work and may be edited as the design evolves.

| Spec | Scope |
|---|---|
| [architecture.md](specs/architecture.md) | Crates, core types, runtime conventions, home layout |
| [data-model.md](specs/data-model.md) | Catalog schema and invariants |
| [cli.md](specs/cli.md) | Commands, exit codes, JSON contract |
| [search.md](specs/search.md) | Query model, FTS5 trigram + LIKE, future vector/hybrid |
| [summarization.md](specs/summarization.md) | Prompts, profiles, providers, budget, `resummarize` |
| [accounts-and-auth.md](specs/accounts-and-auth.md) | Google OAuth, Slack tokens, re-authentication |
| [source-slack.md](specs/source-slack.md) | Slack sync and entries |
| [source-google-meet.md](specs/source-google-meet.md) | Meet discovery strategies and Gemini notes parsing |
| [import-format.md](specs/import-format.md) | Import bundle format (`second-brain-import/v1`) |
| [doctor.md](specs/doctor.md) | Health checks and issues |
| [setup-and-scheduling.md](specs/setup-and-scheduling.md) | Installer, setup wizard, scheduler registration |
| [agent-integration.md](specs/agent-integration.md) | Skill and MCP server |
| [ingest.md](specs/ingest.md) | `sb ingest`: locators, flow, statuses, settings, JSON, security |
| [source-documents.md](specs/source-documents.md) | `google.doc`, `web.page` and `local.file` sources |
| [extract.md](specs/extract.md) | `sb-extract`: formats, encodings, limits, library choice |
| [mvp-plan.md](specs/mvp-plan.md) | Spikes, phases, work items, testing strategy |
