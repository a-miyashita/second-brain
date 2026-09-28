# `sb doctor`

Related ADRs: 0003, 0009, 0011.

`sb doctor [--online] [--fix] [--json]` runs health checks and lists open issues.

- Exit codes: `0` all OK, `1` warnings, `2` errors.
- Without `--online` it makes **no network calls**.

## Checks

| ID | Check | Severity when failing | `--fix` |
|---|---|---|---|
| `home.exists` | `SECOND_BRAIN_HOME` exists and is writable | error | create |
| `home.permissions` | Home `0700`, DB files `0600` (Windows: owner-only ACL) | error | tighten |
| `db.integrity` | `PRAGMA quick_check` | error | — |
| `db.migrations` | Schema is current (binary newer → migrate; DB newer → error) | error | migrate |
| `db.backup` | A backup newer than 30 days is known (setting `backup.last_at`), informational until `sb backup` exists | info | — |
| `raw.consistency` | Entries with `raw_status = present` have their files; segments are contiguous; hashes spot-checked (sample of 50) | warning | mark `missing` |
| `raw.orphans` | Files under `raw/` or `tmp/` not referenced by any `raw_objects` row (left by an interrupted run) | info | delete |
| `sync.queue` | Items in `sync_queue` older than 7 days, or at their attempt limit | warning | — |
| `accounts.status` | No account in `needs_reauth` | error | — (use `sb auth login`) |
| `accounts.online` (`--online`) | Google token refresh works; Slack `auth.test` succeeds; required scopes present | error | — |
| `llm.profiles` | Every profile referenced by `summary.profile.*` exists; the secret or env var is present; the CLI binary is found on `PATH` | error | — |
| `llm.online` (`--online`) | A 1-token test call per used profile. For `local_llm`: server reachability, `start_command` availability, and warm-up latency (includes model load if not loaded) | warning | — |
| `schedule.registered` | Scheduled jobs exist and point to this binary and home | warning | re-register |
| `runs.recent` | The last scheduled `sync` finished within 36 h and was not `failed`. An `interrupted` or `stopped_by_limit` run is reported with the remaining work | warning / error (failed) | — |
| `summaries.backlog` | Count of `pending`/`failed` summaries, and entries at the attempt limit | warning if failed > 0 | — |
| `meet.transcripts` | Share of meet entries without a transcript (informational) | info | — |
| `skills.version` | Installed skills match the binary's embedded version | warning | reinstall |
| `index.consistency` | FTS row count matches the sections count | warning | `index rebuild` |
| `disk.usage` | Size of home; warn when free disk space < 1 GB | warning | — |

After the checks, all **open issues** from the `issues` table are listed, grouped by
severity, with `first_seen_at` / `last_seen_at` and a suggested command (e.g.
`sb auth login acme-slack`).

## Output

Human output uses one line per check (`✔` / `!` / `✖`) and then the issues.

`--json`:

```json
{"schema": "sb.doctor/v1",
 "status": "warning",
 "checks": [{"id": "home.permissions", "status": "ok"},
            {"id": "runs.recent", "status": "warning",
             "message": "last scheduled sync 2 days ago",
             "hint": "check the scheduled task"}],
 "issues": [{"code": "auth.needs_reauth", "severity": "error",
             "account": "acme-slack", "message": "...",
             "hint": "sb auth login acme-slack"}]}
```
