# Changelog

All notable changes to second-brain are written here. The format follows
[Keep a Changelog](https://keepachangelog.com/). Release notes are written when a
release is prepared; see `docs/specs/release.md`.

## Unreleased

## 0.4.0 - 2026-11-13

### Added

- `sb items` lists structured items that are pulled out of each entry when it is
  summarized (ADR-0024): decisions, actions, resolved matters, open questions and
  accomplishments. Filter by `--kind`, `--status`, `--mine`, `--owner`,
  `--asked-by-me`, `--since`/`--until`, `--due-by`, `--stale` and more.
  `--candidates` shows possible resolutions for open actions and questions.
- `sb recent` shows entries by activity time with their summaries, without a keyword
  (for example `sb recent --since today`).
- `--json` output of the new commands: `sb.items/v1` and `sb.recent/v1`.
- Settings `items.min_confidence`, `items.max_per_entry`, `items.stale_days`,
  `items.candidate_days`, `items.candidate_min_similarity` and
  `items.link_min_confidence`.
- `sb` learns which names and emails are yours from your Slack, Google and Microsoft
  accounts, so `--mine` works without setup. Correct them with `extra_aliases` in
  `sb config edit --account <id>`. Only your own Slack email is stored.
- `sb doctor` checks items and aliases.
- The `second-brain` skill is version 5. It answers questions that have no keyword,
  such as "what are my TODOs?" or "what happened this week?". Run
  `sb setup skills --target all` to update it.

### Changed

- New and changed entries are summarized with the v4 prompts, which also return items.
  Stored summaries stay as they are until you run `sb resummarize`; items appear for
  those entries after that.
- Google Meet summaries follow the default summary profile. The built-in
  `summary.profile.google.meet = native` default is removed; set it yourself if you
  want to keep it.
- Catalog migration 0005 adds the `items` and `item_links` tables. It runs on the
  next command.

## 0.3.0 - 2026-11-13

### Added

- `sb ingest` reads files from SharePoint and OneDrive (ADR-0021). Add an account with
  `sb account add microsoft <id> --client-id <GUID>`, then give `sb ingest` a
  SharePoint or OneDrive link. The new source kind is `microsoft.file`. See
  `manuals/en/microsoft-account.md`.
- `sb sync` can read Google Drive, SharePoint and OneDrive folders (ADR-0022). Add a
  folder with `sb folder add <account> <link>`, look at what a sync would add with
  `sb folder check`, and list or remove folders with `sb folder list` and
  `sb folder remove`. The first sync reads the files changed in the last
  `sync.initial_days`; `sb sync --since` reaches further back. No new permission is
  needed. See `manuals/en/folders-and-links.md`.
- `sb sync` can add the documents that Slack threads link to (ADR-0022): Google Docs,
  Sheets, Slides and Drive files, and SharePoint and OneDrive files. It is off by
  default. Turn it on with the setting `links.ingest_kinds`, or per workspace with
  `ingest_links` and `ingest_link_hosts`. `sb scan-links` lists the links and their
  state, and `sb scan-links --ingest` adds the links of threads that were synced before.
  `sb sync --source links` runs only this step.
- The setup and settings screen has a Folders section, and the account editor shows the
  link settings of a Slack workspace and the folders of a Google or Microsoft account.
- `sb doctor` checks the synced folders (`folders.access`, `folders.too_large`), the
  links that wait or were given up (`links.pending`), and Drive files that are both a
  document and a Meet note (`docs.meet_overlap`).
- Summaries of documents get hints (ADR-0023): the folder path of a synced file and a
  short piece of the Slack thread that linked it. The prompt is `document-summary/v3`.
  The agent skill is version 4; run `sb setup skills` again.

### Changed

- Google and Microsoft documents are summarized at any length. The new settings
  `summary.min_chars.<source kind>` set the minimum length for one source kind; the
  default is 0 for `google.doc` and `microsoft.file`. Before, a document shorter than
  `summary.min_chars` (400) was skipped. To keep the old rule, run
  `sb config set summary.min_chars.google.doc 400` and
  `sb config set summary.min_chars.microsoft.file 400`.
- In `sb setup` and `sb config`, the sections after Accounts move by one place, and the
  keys `1` to `9` change the section.
- The catalog gets a new table, `discovered_links` (migration 0004). The migration runs
  on the next command.

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
