# AGENTS.md

Guidelines for coding agents and contributors working **on this repository**. This is
not the guide for agents that *use* second-brain as a knowledge base; that guide is
the skill in `crates/sb-setup/assets/skills/second-brain/`.

## Project in one paragraph

second-brain is a Rust CLI (`second-brain`, alias `sb`) that:

- ingests context (Slack, Google Meet, documents, ...) from multiple accounts;
- stores the raw data as files and a catalog in SQLite;
- summarizes entries with pluggable LLM backends, recording which model produced
  each summary;
- serves search to AI agents through a skill (CLI `--json`) and an MCP server.

## Read first

- [docs/README.md](docs/README.md): index of ADRs and specs.
- ADRs 0002 (data layers) and 0005 (summaries): the two ideas everything else
  depends on.
- [docs/specs/mvp-plan.md](docs/specs/mvp-plan.md): what is in scope now.

## Documentation rules

- **ADRs are immutable once accepted.** Do not rewrite or delete them.
  - To correct details, append `## Amendments` with the date.
  - To change a decision, write a new ADR and mark the old one
    `Superseded by ADR-NNNN` in its Amendments.
- Specs are living documents. Update the spec in the same change as the code that
  diverges from it.
- All documentation, code comments, identifiers, log and CLI messages are in
  **English**. User-facing section labels are localized at display time only
  (`display.language`).

## Repository layout

See [docs/specs/architecture.md](docs/specs/architecture.md). In short:

```text
crates/sb-core      domain types and traits (no I/O)
crates/sb-store     SQLite catalog, raw file store, secrets, sqlite-fts backend
crates/sb-pipeline  ingestion / summarization pipeline
crates/sb-llm       summarizers
crates/sb-google    Google OAuth + Meet/Docs sources
crates/sb-slack     Slack source
crates/sb-extract   document text extraction
crates/sb-mcp       MCP server
crates/sb-setup     setup, scheduler registration, skill install
crates/sb-cli       binaries `second-brain` and `sb`
assets/             Slack app manifest (the skill files are in crates/sb-setup/assets/)
```

Dependency direction: `sb-core` ← sources / llm / store ← `sb-pipeline` ← `sb-cli` /
`sb-mcp`. Library crates never print to stdout.

## Build and test

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs these on Windows, macOS and Linux. Release builds use cargo-dist.

## Coding conventions

- Rust edition 2024. The MSRV is pinned in the workspace `Cargo.toml`
  (`rust-version`).
- Errors:
  - `thiserror` enums in library crates, `anyhow` only in `sb-cli`;
  - no `unwrap()` / `expect()` outside tests, except for invariants proven locally
    with a comment.
- Async: `tokio`. HTTP: `reqwest` with `rustls` only; never enable OpenSSL features.
  SQLite: `rusqlite` with `bundled`.
- Logging with `tracing`:
  - **Never log secrets or message bodies at `info` or above.** Tokens and keys go
    through the redacting `Secret` newtype, which has no `Display`.
  - Personal content (Slack messages, meeting text) is logged only at `trace`.
- Keep `Source::normalize` pure and deterministic. It must not touch the network or
  the clock; inject `Clock` where time is needed.
- Every `--json` output carries a `schema` field. Adding fields is fine. Renaming,
  removing or changing the meaning of a field requires a version bump (see
  [cli.md](docs/specs/cli.md)).
- Schema changes are new numbered migrations. Never edit an applied migration.
- Handle paths cross-platform: use `Path`/`PathBuf`, never string concatenation.
  Remember Windows ACLs where Unix code sets modes.

## Tests

- **No real network access in tests.** Use `wiremock` for HTTP APIs and stub
  executables for LLM CLIs.
- Fixtures under `tests/fixtures/` must contain **no real personal data**. Scrub
  names, emails, channel names, IDs and message text from captured API responses.
- Store tests use a temporary `SECOND_BRAIN_HOME`.
- Platform-specific registration code (Task Scheduler XML, launchd plist, crontab
  block) is tested by rendering, not by installing.

## Security and privacy

- The catalog holds credentials. Files must be `0600` (Windows: owner-only ACL) and
  the home directory `0700`. See ADR-0003.
- Never commit real config, tokens, OAuth client JSON or data exports. `.gitignore`
  must cover them.
- The MCP server exposes write operations (`ingest`) only when
  `mcp.allow_ingest` is true.

## Commits

- Use small, focused commits with a conventional prefix (`feat:`, `fix:`, `docs:`,
  `refactor:`, `test:`, `chore:`), in English.
- Reference the ADR or spec when a change implements or amends one.
