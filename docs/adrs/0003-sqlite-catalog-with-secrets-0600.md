# ADR-0003: SQLite catalog, including secrets, protected by file permissions

- Status: Accepted
- Date: 2026-09-29

## Context

second-brain needs persistent storage for settings, accounts, ingestion state and
entries. It also needs somewhere to keep credentials: OAuth refresh tokens, Slack
user tokens and LLM API keys.

OS keychains were considered. However, the Linux Secret Service is usually
unreachable from cron, and credential storage would then differ per OS. A DB file
protected by file permissions is a simpler, uniform alternative. Its residual risk
is accepted: another process running as the same OS user can read the DB.

## Decision

- The catalog is a single SQLite database, `$SECOND_BRAIN_HOME/second-brain.db`,
  opened with `rusqlite` (bundled SQLite, WAL mode).
- Settings, accounts, sync state, entries, sections, summary generator info, run
  history and **secrets** all live in it. The schema is in
  [specs/data-model.md](../specs/data-model.md).
- The DB file and its `-wal`/`-shm` side files are restricted to the owning user:
  - Unix: mode `0600` on the files, `0700` on `$SECOND_BRAIN_HOME`.
  - Windows: an ACL granting access only to the current user (inheritance
    removed), applied when the file is created.
- `sb doctor` verifies these permissions and reports an error if they are looser.
- Secrets are never printed. `sb config get` shows secrets masked, and logs redact
  them.
- The catalog store is fixed to SQLite. Only the **search backend** is pluggable
  (ADR-0004).
- Standard environment variables (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`,
  `GEMINI_API_KEY`, `SLACK_USER_TOKEN`) are honoured only as a fallback when the
  corresponding secret is not stored.

## Consequences

- Behaviour is the same on every OS and in unattended runs. There is no keychain
  prompt and no D-Bus dependency.
- Anyone who copies the DB file gets the credentials. The docs must say so, and
  backups must be treated as secret.
- Schema migrations are managed by an embedded, ordered list of SQL migrations
  tracked in the `schema_migrations` table.

## Alternatives considered

- **OS keychain via the `keyring` crate**: better at-rest protection, but
  unreliable for cron on Linux and adds per-OS failure modes. It could be added
  later as an optional secret store behind the same interface.
- **Encrypting secrets with a key file next to the DB**: adds complexity without
  real protection, because the key sits beside the data.
