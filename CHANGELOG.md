# Changelog

All notable changes to second-brain are written here. The format follows
[Keep a Changelog](https://keepachangelog.com/). Release notes are written when a
release is prepared; see `docs/specs/release.md`.

## Unreleased

### Added

- Initial release. second-brain is a command-line tool (`second-brain`, short alias
  `sb`) that builds a personal knowledge base for AI agents. Install it with
  `cargo install second-brain --locked`.
- Collect your Slack conversations and Google Meet notes from several accounts with
  `sb sync`, once or on a daily schedule (`sb setup schedule`).
- Add Google Docs, web pages and local files (text, CSV, HTML, docx, pptx, xlsx, PDF)
  with `sb ingest`, and import existing data with `sb import`.
- Keep the raw data as files and a catalog in SQLite. Search it with `sb search`, and
  read it with `sb show`.
- Summarize each entry with an LLM and record which model wrote each summary.
  Summarizers: the Anthropic API, OpenAI-compatible servers, and the Claude, Copilot,
  Codex and Antigravity command-line tools. Weekly and monthly budget caps limit the
  cost.
- Install an agent skill with `sb setup skills`, so that AI agents can search your
  entries through the CLI (`--json` output).
- Check the health of the setup with `sb doctor`.
