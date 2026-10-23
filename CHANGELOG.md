# Changelog

All notable changes to second-brain are written here. The format follows
[Keep a Changelog](https://keepachangelog.com/). Release notes are written when a
release is prepared; see `docs/specs/release.md`.

## Unreleased

## 0.2.0 - 2026-10-23

### Added

- `sb config` without a subcommand opens a settings screen in the terminal
  (ADR-0020). It edits settings, account configs and secrets with the same checks
  as `sb config set`.
- `sb config edit --editor` keeps the TOML-in-`$EDITOR` flow.

### Changed

- `sb setup` opens a setup screen in the terminal instead of the question-by-question
  wizard (ADR-0020). Sections can be done in any order and again later.
- `sb config edit [--account <id>]` opens the settings screen instead of `$EDITOR`.
- `sb setup` and `sb config` without a terminal (or with `--json`) are usage errors;
  use `sb setup --yes`, the `sb setup` steps or the `sb config` subcommands.

## 0.1.0 - 2026-10-09

Initial release.
