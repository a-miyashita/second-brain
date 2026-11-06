# Changelog

All notable changes to second-brain are written here. The format follows
[Keep a Changelog](https://keepachangelog.com/). Release notes are written when a
release is prepared; see `docs/specs/release.md`.

## Unreleased

### Added

- `sb ingest` reads files from SharePoint and OneDrive (ADR-0021). Add an account with
  `sb account add microsoft <id> --client-id <GUID>`, then give `sb ingest` a
  SharePoint or OneDrive link. The new source kind is `microsoft.file`. See
  `manuals/en/microsoft-account.md`.

## 0.2.1 - 2026-11-03

### Fixed

- Reading `.xlsx` files now checks the declared cell range of every XML part in the
  file, not only those under `xl/worksheets/`, so a crafted workbook cannot skip the
  check and make extraction use too much memory.
- The `xlsx` feature of `second-brain-extract` now builds on its own.

### Security

- Updated `zip` to 9.3 and `calamine` to 0.38 (CVE-2026-115905, zip cloaking). The
  advisory affects `ZipStreamReader`, which second-brain does not use, so it is
  probably not exploitable here; the update is a precaution.

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
