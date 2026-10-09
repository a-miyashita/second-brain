# second-brain

second-brain is a command-line tool (`second-brain`, short alias `sb`) that builds a
personal knowledge base for AI agents.

The tool does the following:

- It collects context from Slack, Google Meet notes, documents and web pages.
- It stores the raw data as files and keeps a catalog in SQLite.
- It summarizes each entry with an LLM and records which model wrote each summary.
- It lets AI agents search the catalog through an agent skill that calls the CLI
  with `--json`.

## Build and test

You need Rust 1.88 or later. The project uses Rust edition 2024.

```sh
cargo build --workspace
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same checks on Linux, macOS and Windows. CI also runs `cargo check` on
Rust 1.88.

The tests use no real network. HTTP APIs are replaced with `wiremock`, and LLM CLIs
are replaced with stub executables.

## Try it without touching your own data

The tool keeps all its data in one directory. The variable `SECOND_BRAIN_HOME` sets
this directory. Point it at a temporary location to try the tool safely:

```sh
export SECOND_BRAIN_HOME="$(mktemp -d)/home"
cargo run --bin sb -- version
cargo run --bin sb -- setup home     # creates the directory and the catalog
cargo run --bin sb -- doctor         # shows the health of the setup
```

Without `SECOND_BRAIN_HOME`, the tool uses a default directory for your OS. Sync
and `ingest` need real Slack or Google accounts. Use a test account or a temporary
home when you try them.

To install the binaries from your working copy (users install with
`cargo install second-brain --locked`; see [release.md](docs/specs/release.md)):

```sh
cargo install --path crates/sb-cli --locked   # installs second-brain and sb
```

## Repository layout

```text
crates/sb-kernel    domain types and traits (no I/O)
crates/sb-store     SQLite catalog, raw file store, secrets, FTS5 search backend
crates/sb-pipeline  ingestion and summarization pipeline
crates/sb-llm       summarizers (LLM APIs and LLM CLIs)
crates/sb-google    Google OAuth, Meet and Docs sources
crates/sb-slack     Slack source
crates/sb-extract   text extraction from documents
crates/sb-ondemand  sources that need no sync: web pages and local files
crates/sb-setup     setup, scheduler registration, skill install
crates/sb-cli       the binaries `second-brain` and `sb`
assets/             Slack app manifest (the agent skill is in crates/sb-setup/assets/)
docs/               ADRs and specs
```

The dependency direction is: `sb-kernel` ← sources, `sb-llm` and `sb-store` ←
`sb-pipeline` ← `sb-cli`. Library crates never print to stdout.

## How the data flows

```text
sources (Slack, Google, web, files)
   │ fetch
   ▼
raw files ──normalize──▶ catalog (SQLite: entries, sections, summaries)
                            │            ▲
                            │            └── summarize (LLM)
                            ▼
                      search index (FTS5)
                            │
                  sb search / sb show  ◀── agents (skill, `--json`)
```

Raw files are the source of truth. The catalog and the index can be built again
from them. Every summary records the model and the prompt version that made it, so
you can replace summaries with a better model later.

## Design documents

Read these documents before you change the design:

- [docs/README.md](docs/README.md): index of all ADRs and specs.
- ADR-0002 (data layers) and ADR-0005 (summaries). The other design depends on them.
- [docs/specs/architecture.md](docs/specs/architecture.md): crates and conventions.
- [docs/specs/mvp-plan.md](docs/specs/mvp-plan.md): what is in scope and what is done.

ADRs record decisions. After an ADR is accepted, do not rewrite it. Add an
`## Amendments` section instead. Specs are living documents. Update a spec in the
same change as the code.

## Contributing

[AGENTS.md](AGENTS.md) has the full rules. These are the main ones:

- Write all documentation, code comments, identifiers and messages in English.
- Do not use `unwrap()` or `expect()` outside tests.
- Never log secrets. Never log message bodies at `info` level or above.
- Add schema changes as new numbered migrations. Never edit an applied migration.
- Every `--json` output has a `schema` field.
- Do not use real network access or real personal data in tests and fixtures.
- Use small commits with a prefix such as `feat:`, `fix:` or `docs:`.

## Security

The catalog database holds credentials: OAuth refresh tokens, the Slack token and
API keys. The tool sets the files to owner-only access. Treat copies and backups of
the data directory as secrets.

Never commit real configuration, tokens, OAuth client files or data exports.

## License

[MIT](LICENSE)
