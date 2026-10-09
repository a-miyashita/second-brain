# MVP scope and implementation plan

## Phase 0 — spikes (verify before committing code paths)

| ID | Question | Outcome feeds |
|---|---|---|
| S1 | Does Meet v2 `conferenceRecords.list` + `smartNotes` return meetings I attended but did not organize? What scopes and admin settings does it need? | Whether `meet_api` becomes the default strategy |
| S2 | Confirm the Gemini notes format as exported by Drive `text/markdown`. Check: section headings (observed to be Japanese even for English meetings), the embedded transcript heading, the survey line, timestamp anchors, the Invited line with struck-through absentees, and the auto-generated title patterns | Parser rules in source-google-meet.md |
| S3 | Can cargo-dist install to `%LOCALAPPDATA%\Programs\second-brain\bin` and `~/.local/bin` and update `PATH`? | Custom installer, or not |
| S4 | Copilot CLI non-interactive flags, JSON output, skill directory, MCP config path | summarization.md and agent-integration.md |
| S5 | Windows Task Scheduler without a console window flash | setup-and-scheduling.md |
| S6 | Which pure-Rust libraries extract text well enough from PDF (Japanese and English), docx, pptx, xlsx and HTML, and survive malformed files? (phase 2, for `sb ingest`) | Library choice and PDF scope in extract.md |

The results are recorded as amendments to the relevant specs, or as new ADRs if a
decision changes.

## Phase 1 — MVP

Goal: a usable daily tool for Slack and Meet, searched through the skill from
terminal agents (Copilot CLI, Claude Code), with import of existing data.

| # | Work item | Depends on |
|---|---|---|
| 1 | Workspace skeleton, CI (fmt, clippy, test on Windows/macOS/Linux), cargo-dist release config | — |
| 2 | `sb-core` types and traits | 1 |
| 3 | `sb-store`: migrations, schema, permissions (Unix mode, Windows ACL), settings, secrets, raw file store | 2 |
| 4 | `sqlite-fts` search backend, including the short-term LIKE path; `sb search/show/list/stats/review`; `sb index rebuild` | 3 |
| 5 | `sb-pipeline`: ingestion pipeline, run records, issues, sync lock, chunked commits, `sync_queue`, cancellation and graceful stop, limits (`--max-*`, `--time-limit`) | 3 |
| 6 | `sb import` (import-format v1) + `--dry-run` | 3, 4, 5 |
| 7 | `sb-llm`: Anthropic API, OpenAI(-compatible), Claude CLI, Copilot CLI; prompts; map-reduce; `sb summarize`, `sb resummarize`, `--estimate` | 5 |
| 8 | `sb-slack`: account add/auth, sync (normal/deep), normalize, `--estimate` | 5, 7 |
| 9 | `sb-google`: OAuth (PKCE loopback), Drive/Calendar clients, `google.meet` source with `calendar` and `drive` strategies, Gemini notes parser | 5 |
| 10 | `sb refetch`, `sb reextract` | 8, 9 |
| 11 | `sb auth status/login`, `sb account *`, `sb config *` | 3, 8, 9 |
| 12 | `sb doctor` (all checks in doctor.md except those for later-phase features) | 3–11 |
| 13 | `sb-setup`: `setup home/llm(basic)/schedule/skills/env`, and the wizard | 3, 7, 11 |
| 14 | Skill content (`SKILL.md` + references) | 4 |
| 15 | README user guide: Google OAuth client and Slack app creation | 8, 9 |

Items 8 and 9 can proceed in parallel after item 5. Item 6 (import) should land
early, so real imported data is available for testing search.

**MVP exit criteria:**

- An import bundle of existing data has been imported, and a scheduled daily sync
  on Windows ingests new Slack threads and Meet notes.
- `sb search` answers a sample question set (decisions, owners, history) with the
  expected entries in the top results.
- `sb doctor` is clean.

### Status (2026-10-06)

| # | State |
|---|---|
| 1 | Workspace and CI done. `dist-workspace.toml` is written; the release workflow still has to be generated with `dist generate` |
| 2–9 | Done, with the tests described below (mock HTTP servers, stub executables, resume and incremental tests) |
| 10–13 | Done (`sb refetch`/`reextract`, `sb auth`/`account`/`config`, `sb doctor`, `sb setup` and the wizard) |
| 14 | Done (`crates/sb-setup/assets/skills/second-brain/`) |
| 15 | Done (README guides) |
| Phase 2: `sb ingest` | Done: `google.doc`, `web.page`, `local.file`, `sb-extract` (ADR-0014, [ingest.md](ingest.md)). Spike S6 resolved for everything except real-world PDFs, see [extract.md](extract.md) |

Not done yet:

- Phase 0 spikes S1–S5 need real accounts and machines. Choices made without them
  are marked in the specs: `copilot -p` via stdin (S4), `conhost --headless` (S5),
  the Windows install path (S3) and the Gemini notes format (S2, parser built from
  the documented observations).
- The MVP exit criteria (import of real data, a scheduled daily sync on Windows,
  a sample question set, a clean `sb doctor`) need the user's data.
- The Gemini API summarizer is left for phase 2 (it was listed under both).

## Phase 2

- MCP server + `setup mcp`.
- `sb ingest` with `google.doc` (Drive export and `sb-extract`), `local.file` and
  `web.page`: **done**, see [ingest.md](ingest.md).
- `scan-links`.
- Gemini API summarizer. (The Foundry Local wizard was dropped, ADR-0015.)
- Notification sinks `slack_dm` and `desktop` (needs `chat:write` in the manifest).
- `meet_api` strategy (if S1 is positive).

## Phase 3

- Email: `mail.*` via Gmail API, then IMAP.
- Vector search (`sqlite-vec`, `Embedder` trait, hybrid RRF).
- `sb backup`.
- GUI decision: a Tauri app over the library crates, starting with settings and
  the LLM wizard.

## Testing strategy

- **Unit tests:** parsers (Gemini notes, Slack rendering) run against fixtures.
  Fixtures are real API responses with personal data replaced.
- **HTTP clients:** tested against a local mock server (`wiremock`). **No real
  network calls in tests.**
- **Store tests:** a temp-dir home, with migrations applied from scratch.
- **Resume tests (per source):**
  - Run against a mock API.
  - Cancel after N commits, including once in the middle of a chunk.
  - Resume, and assert the final catalog equals that of an uninterrupted run.
  - Assert that no summary is requested twice for the same `input_hash`.
- **Incremental Slack test:** a thread gains replies between runs. Assert that the
  second fetch requests only `oldest = last_ts`, that it writes one new segment,
  and that the summary is regenerated once.
- **Summarizers:** a fake `Summarizer` for pipeline tests. CLI providers are tested
  with stub executables on `PATH`.
- **Platform-specific code** (permissions, scheduler registration) is tested on the
  matching CI OS. Registration tests use a dry-run mode that renders the XML, plist
  or crontab without installing it.
