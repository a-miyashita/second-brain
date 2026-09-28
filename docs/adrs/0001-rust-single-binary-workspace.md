# ADR-0001: Rust workspace producing a single `second-brain` binary

- Status: Accepted
- Date: 2026-09-29

## Context

Personal knowledge-base tooling is often a pile of scripts: a script runtime with
packages, external sync tools, OS-specific scheduling, and a separate setup
procedure per data source. Such tooling is hard to install and maintain for anyone
but its author.

second-brain must provide:

- context ingestion (single item and scheduled batch),
- context search,
- a batch entry point for cron / launchd / Task Scheduler,
- re-authentication,
- setup / installation,
- an agent skill and an MCP server,
- optionally a configuration GUI.

It must run on Windows, macOS and Linux.

## Decision

- Implement in **Rust** (edition 2024), as a Cargo workspace.
- Ship **one binary**, `second-brain`, with subcommands. The same `main` is also
  built as a second binary target named `sb`, a short alias. There is no
  separate executable per tool.
- Split the code into library crates by responsibility. The binary crate is a
  thin shell. The crate layout is in [specs/architecture.md](../specs/architecture.md).
- Link statically where the platform allows:
  - SQLite via `rusqlite` with the `bundled` feature,
  - TLS via `rustls` (no OpenSSL),
  - `x86_64/aarch64-unknown-linux-musl` for Linux,
  - a static CRT for Windows MSVC.

  On macOS, only system frameworks are linked dynamically.
- If a GUI is built later, it is a separate binary/crate that calls the same library
  crates. It is never required.

## Consequences

- Installing is copying one file (plus the alias). Nothing else needs to be
  installed at runtime, except optional external tools the user chooses: an LLM CLI
  or a local LLM server.
- Every feature is reachable from the CLI, so agents, schedulers and a future GUI
  share one code path.
- Heavy optional functionality, such as document text extraction, sits behind Cargo
  features so the default binary stays reasonably small.
- Rust ecosystem gaps (for example, no official Anthropic or Google SDKs) are
  covered by thin in-house HTTP clients built on `reqwest`.

## Alternatives considered

- **One binary per tool** (`sb-ingest`, `sb-search`, ...): more files to install and
  put on `PATH`, and duplicated argument parsing, for no real benefit.
- **Python or another scripting language**: needs a runtime and package
  dependencies on every machine, which is exactly the installation burden to
  avoid.
