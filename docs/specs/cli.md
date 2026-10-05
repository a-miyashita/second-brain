# CLI specification

Related ADRs: 0001, 0009, 0010.

The binary is `second-brain`, with alias `sb`. Examples use `sb`.

## Global options

| Option | Meaning |
|---|---|
| `--home <dir>` | Override `SECOND_BRAIN_HOME` |
| `--json` | Machine-readable output (see "JSON contract") |
| `-v`, `-q` | Verbosity |
| `--no-color` | |

## Commands

Phase: M = MVP, 2 = phase 2, 3 = phase 3. See [mvp-plan.md](mvp-plan.md).

### Setup and configuration

| Command | Phase | Description |
|---|---|---|
| `sb setup [--yes]` | M | Interactive wizard running the steps below in order |
| `sb setup home` | M | Create home directory, DB, permissions, pseudo-accounts |
| `sb setup llm` | M (basic) / 2 (Foundry Local wizard) | Create or edit summarizer profiles, test them |
| `sb setup schedule [--time HH:MM] [--deep-day DAY --deep-time HH:MM] [--systemd] [--remove]` | M | Register or remove scheduled jobs |
| `sb setup skills --target copilot\|claude\|codex\|all [--remove]` | M | Install agent skill files |
| `sb setup mcp --client <name> [--apply]` | 2 | Print or apply MCP client config |
| `sb setup env` | M | Persist `SECOND_BRAIN_HOME` when a non-default home is used |
| `sb config list\|get <key>\|set <key> <value>\|unset <key>` | M | Global settings; secrets masked |
| `sb config set-secret <name> [--account <id>]` | M | Store a secret read from a hidden prompt or stdin |
| `sb config edit [--account <id>]` | M | Open settings or an account's config as TOML in `$EDITOR`, validate, write back |

### Accounts and authentication

| Command | Phase | Description |
|---|---|---|
| `sb account add google <id> --client-secret <file> [--label ..] [--features meet,docs,gmail]` | M | Import OAuth client and run consent |
| `sb account add slack <id> [--token xoxp-..] [--label ..]` | M | Validate token (`auth.test`) and store |
| `sb account add imap <id> ...` | 3 | |
| `sb account list` / `show <id>` / `disable <id>` / `enable <id>` / `remove <id> [--purge]` | M | `--purge` also deletes the account's entries and raw files |
| `sb auth status` | M | Validity of every account's credentials; exit 1 if any need re-auth |
| `sb auth login <id>` | M | Re-authenticate: Google consent again, or Slack token replacement |

### Ingestion

| Command | Phase | Description |
|---|---|---|
| `sb sync [--account ..] [--source ..] [--deep] [--since DATE] [--no-summary] [--max-summaries N] [--max-cost USD] [--time-limit DUR] [--dry-run] [--estimate]` | M | Incremental, resumable sync of all enabled accounts and sources, then pending summaries, then indexing. Interrupt any time; re-run to continue (ADR-0012) |
| `sb ingest <url-or-path>... [--account ..] [--title ..] [--context ..] [--date ..] [--force]` | 2 | Single-item ingest (Google Docs/Drive, URL, local file). `--context` becomes the `background` section |
| `sb refetch [filters] [--raw-missing]` | M | Fetch raw data again by natural key; enables re-summarization of imported entries |
| `sb reextract [filters]` | M | Re-run `normalize` on stored raw data (no network) |
| `sb summarize [--max-summaries N] [--max-cost USD] [--time-limit DUR] [--retry-failed] [--profile ..]` | M | Process entries with `summary_status = pending/failed` (below the attempt limit) |
| `sb resummarize [filters] [--profile P \| --native] [--where-model M] [--dry-run] [--estimate] [--limit N] [--max-cost USD] [--time-limit DUR] [--force]` | M | Overwrite generated sections for matching entries. Entries already at the target generator are skipped, so re-running resumes (see [summarization.md](summarization.md)) |
| `sb import <bundle-dir> [--map kind=account]... [--dry-run]` | M | Import a bundle (see [import-format.md](import-format.md)) |
| `sb scan-links [--since ..] [--account ..]` | 2 | List Google Docs links in Slack entries that have decisions or action items and are not yet ingested |

Common filters: `--account`, `--source <source_kind>`, `--since`, `--until`,
`--entry <uid>` (repeatable), `--raw-status`, `--summary-status`.

### Retrieval

| Command | Phase | Description |
|---|---|---|
| `sb search <term>... [--section K] [--source ..] [--account ..] [--since ..] [--until ..] [--limit N]` | M | See [search.md](search.md) |
| `sb show <entry_uid> [--section K]... [--raw [--role R]] [--meta]` | M | Print an entry (Markdown by default). `--raw` prints the raw file path(s) and, with `--role`, the content |
| `sb list [filters] [--limit N]` | M | List entries, newest first |
| `sb stats` | M | Counts by account, source, raw status and summary status/model, and summarization spend against the weekly and monthly budget (ADR-0013) |
| `sb budget [--weeks N] [--months N] [--by-model]` | M | Summarization spend (computed by this tool) against the weekly and monthly budget: current periods, history, total and breakdown by model. See [summarization.md](summarization.md#history) (ADR-0013) |
| `sb review [filters] [--channel ..] [--limit N] [--detail-lines N] [--full] [--all]` | M | Show generated sections next to the source text (Slack: the rendered conversation; Meet: the transcript excerpt) to check summary quality by eye. Newest first; `--all` includes entries without summaries. Also useful before and after `sb resummarize` |

### Operations

| Command | Phase | Description |
|---|---|---|
| `sb doctor [--online] [--fix]` | M | See [doctor.md](doctor.md) |
| `sb index rebuild` | M | Rebuild the active search backend(s) |
| `sb mcp` | 2 | Run the MCP server over stdio |
| `sb version` | M | Version, build target, skill version |

### Implementation notes

Options added during implementation (all additive):

- Hidden global `--trigger schedule`: marks a run as scheduled where the job
  definition cannot set `SB_TRIGGER` (Windows Task Scheduler).
- `sb search --all-sections`: every matching section instead of the best one per
  entry (search.md).
- `sb setup llm [--preset anthropic|openai|claude_cli|copilot_cli|local] [--name N]
  [--model M] [--base-url URL] [--no-default] [--no-test]` for non-interactive use.
- `sb setup schedule --dry-run` prints the task XML, plists, crontab block or
  systemd units without installing them.
- `sb account add google --no-browser`, `sb auth login <id> --no-browser`,
  `sb auth status --online`, `sb account remove <id> --purge --yes`.
- `sb summarize --estimate`.
- Error codes in `sb.error/v1` include `usage`, `home.not_initialized`,
  `sync.locked`, `entry.not_found`, `account.not_found` and `failed`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Completed with problems. For `sync`: partial. For `doctor`: warnings. For `auth status`: re-auth needed |
| 2 | Failure (`doctor`: errors) |
| 64 | Usage error |
| 75 | Another sync holds the lock (temporary failure) |
| 130 | Interrupted by a signal (Ctrl+C / SIGTERM). Committed work is kept; re-run to continue |

A run that stops because of `--max-*` / `--time-limit` exits with 0 and reports
what remains.

## Interruption (ADR-0012)

- The first Ctrl+C / SIGINT / SIGTERM (Windows: Ctrl+C, Ctrl+Break, console close)
  starts a graceful stop:
  - no new work is started;
  - in-flight work is committed within `pipeline.shutdown_grace_secs`;
  - the run is recorded as `interrupted`.
- A second signal aborts immediately.
- Every long-running command (`sync`, `summarize`, `resummarize`, `refetch`,
  `import`, `index rebuild`) follows these rules.

## JSON contract

- Every `--json` output is a single object:
  - `{"schema": "sb.<command>/v1", ...}` on success;
  - `{"schema": "sb.error/v1", "error": {"code", "message"}}` on failure.
- Fields may be added without a version bump. Removing or renaming a field, or
  changing its meaning, bumps `vN`.
- Timestamps are RFC 3339.
- Section kinds use the English keys. Human-readable output localizes labels
  according to `display.language` (`ja` / `en`; defaults to the OS locale).

`sb search --json` example:

```json
{
  "schema": "sb.search/v1",
  "query": {"terms": ["CSV"], "section": "decisions", "limit": 8},
  "hits": [{
    "entry_uid": "01J9...",
    "title": "Daily Dev Standup",
    "source_kind": "google.meet",
    "account": {"id": "work-google", "label": "Work"},
    "date": "2026-09-03T00:15:00Z",
    "section": "decisions",
    "snippet": "... will proceed with CSV ...",
    "score": 12.3,
    "cite_url": "https://docs.google.com/document/d/.../edit"
  }]
}
```

`cite_url` is `source_url`, falling back to `metadata.transcript_url`, then the
local raw path.
