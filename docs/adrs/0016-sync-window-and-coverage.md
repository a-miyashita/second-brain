# ADR-0016: Sync window — a short first window, forward cursors, and tracked backward coverage

- Status: Accepted
- Date: 2026-10-06

## Context

ADR-0012 made sync resumable and incremental, but it left the *depth* of the first
sync to per-source settings that do not agree:

| Source | First sync | Daily run |
|---|---|---|
| Slack | 365 days (`backfill_days`) per conversation | from the per-conversation cursor |
| Meet, Drive strategy | 30 days (`drive_backfill_days`) | from the `modified_after` cursor |
| Meet, Calendar strategy | 3 days (`calendar_days`) | **a fixed 3-day window every run**; the stored `last_time_max` is never read |

Problems:

- The first sync (started from `sb setup`) is the most expensive run there will ever
  be, because it summarizes everything it fetches (ADR-0013 estimates $4–$7). A year of
  Slack makes it long and costly before the user has seen any result or tuned the
  settings.
- Three keys with three meanings configure what is really one idea.
- Going further back later has no proper mechanism. `--since` re-fetches from the
  given date up to now, repeating the already-covered part (rate limits and time on
  Slack), and on the Drive strategy it can move the cursor backwards.
- Nothing records how far back the data actually reaches, so neither the user nor an
  agent can tell whether "no result" means "nothing happened" or "not synced yet".

## Decision

### 1. One initial window: `sync.initial_days` (default 30)

- A single setting replaces `backfill_days`, `drive_backfill_days` and the first-run
  role of `calendar_days`. It is global (`config.toml`), and an account may override
  it with `initial_days` in `accounts.config`.
- It applies to every scope that has no cursor yet: the first sync of an account,
  and a Slack conversation that appears later (new DM, newly joined channel, new
  `full_channels` entry). The scope starts at `run_started_at - initial_days`.
- `sb setup` shows the value in the first-sync step and lets the user change it
  before `sb sync --estimate`. `--yes` keeps the default.
- The old keys are **ignored** (existing rows keep them harmlessly) and `sb doctor`
  reports them as `config.legacy_keys` (info). Existing accounts already have
  cursors, so only conversations that appear later are affected.

### 2. Daily runs continue from the previous run's start

- Each run captures one instant, `run_started_at`, and uses it as the upper bound of
  everything it fetches. The forward cursor of a scope (`covered_until`) is advanced
  to `run_started_at - sync.overlap_secs` (default 300), never beyond committed data
  (ADR-0012 rule 2 still holds). The overlap absorbs clock skew between this machine
  and the APIs. Re-fetching it is harmless because every fetch is idempotent.
- Slack and the Drive strategy already work this way; only the cursor semantics are
  named and made uniform.
- The **Calendar strategy** moves from a fixed window to a cursor. Its window is
  `[max(covered_until - calendar_overlap_days, initial start), run_started_at + 1h]`.
  The overlap (default 3 days, replacing `calendar_days`) is needed because Gemini
  notes are attached to an event *after* it ends; a strict "since the last run"
  window would miss a meeting that ended just before the previous run and was
  documented just after it. Unchanged notes are skipped through the stored
  `modifiedTime` (source-google-meet.md), so the overlap costs only a list call.

### 3. Backward coverage: `covered_since`

Every scope keeps the interval it has fetched: `[covered_since, covered_until]`. It is
always **one contiguous interval**.

- A scope is: a Slack conversation, the Meet `drive` strategy, the Meet `calendar`
  strategy. The values live in the scope's existing `sync_state` JSON value (no schema
  change, no migration).
- `covered_since` is set to the start of the initial window on first sync, and moves
  back only when a backward fetch completes.
- A missing `covered_since` (a cursor written before this ADR) means *unknown*. It is
  treated as equal to `covered_until`, so the first backward run re-fetches from the
  requested date. That repeats some work once, and never claims coverage that is not
  known to exist.
- **Slack day alignment.** `slack.day` entries only append newer messages
  (source-slack.md), so a day must not be fetched in two pieces from the backward
  side. For Slack, the start of the initial window and the `--since`/`--until`
  bounds are rounded down to local midnight in `slack.day_timezone`, and
  `covered_since` is always such a midnight.

### 4. `sb sync --since DATE [--until DATE]` — extending backwards

- `--since X` alone means **extend coverage back to X**. For every selected scope,
  the range `[X, covered_since)` is fetched, then `covered_since` becomes `X`. Scopes
  already covered back to X are skipped. The normal forward step runs first, so one
  command does both. Example: after a 30-day setup, `sb sync --since 90d` fetches only
  days 31–90.
- `--since X --until Y` is an **explicit window** `[X, Y)`, fetched regardless of
  coverage (repair after an outage, or re-fetching a period). Coverage is updated only
  when the window reaches the covered interval (`Y >= covered_since`, in which case
  `covered_since` becomes `min(covered_since, X)`). A detached window is fetched but
  not recorded, so coverage never claims a hole; the command says so.
- `--until` without `--since` is a usage error. `Y` must not be later than
  `run_started_at`.
- `--since` and `--until` accept `YYYY-MM-DD`, RFC 3339, or a relative age such as
  `90d` or `12w` (age before the run start).
- The range is processed in the same bounded windows as any back-fill (ADR-0012: 7
  days for Slack), each committed with its `covered_since` update, so an interrupted
  extension keeps what it did and repeats at most one window. The cursor can never
  move backwards past data that was not committed.
- Extension ignores the dormant-conversation rule: old history is exactly what a
  dormant conversation holds. Slack involvement search is bounded by `after:`/`before:`
  of the range.
- `--estimate` and `--dry-run` honour `--since`/`--until`, so the cost of "go back to
  90 days" can be seen before it is paid. Budget caps (ADR-0013) apply as always.
- The Drive strategy must not lower its forward cursor: backward ranges use their own
  query bounds (`modifiedTime > X and modifiedTime <= Y`) and never write
  `modified_after`.

### 5. Visibility

`sb stats` shows, per account and source, `covered_since` (the start still guaranteed
across all scopes, i.e. the latest one) and `covered_until`. `sb search`
results are unchanged; the skill tells agents to check coverage when asked about a
period (agent-integration.md).

## Consequences

- The first sync is smaller and cheaper (default 30 days instead of 365 for Slack) and
  consistent across sources. Users who want more history ask for it deliberately and
  see the estimate first.
- Existing Slack data older than 30 days stays; nothing is deleted or re-summarized.
  Only the *default for future new conversations* changes.
- One contiguous interval per scope is a simplification: it cannot represent "last 30
  days plus last January". That is accepted. Detached windows are still possible but
  untracked.
- `--since` changes meaning: from "back-fill from this date to now" to "extend
  backwards, skipping what is covered". The old behaviour is available as an explicit
  window: `--since X --until now`.
- The Calendar strategy gains a cursor and an overlap setting, and the Slack and
  Meet adapters need the `run_started_at` / coverage plumbing described in
  [specs/sync-window-plan.md](../specs/sync-window-plan.md).
- ADR-0012 is not superseded: its resumability rules are the foundation of this
  design. Its mention of "a year of Slack" is a description of the old default and is
  recorded in its Amendments.

## Amendments

### 2026-10-06: details settled during implementation

- `--estimate` and `--dry-run` do not fetch anything (they estimate pending
  summaries and list queued items), so `--since` / `--until` do not change what
  they report. The sentence in section 4 that they "honour" the range is not
  implemented; the options are validated only.
- Backward ranges are fetched **newest window first** (Slack: 7 local days per
  window), because a committed window extends the contiguous coverage only when it
  touches it. Sections 3 and 4 do not depend on the order.
- The `coverage` JSON of `sb stats` and `sb sync` is a list of
  `{account, source, covered_since, covered_until}`; `sb stats` has no separate
  `sb status` command (the "Visibility" section means `sb stats`).
- `config.legacy_keys` is informational and has no automatic fix; the user removes
  the keys with `sb config edit --account <id>`.
- The Meet Calendar strategy commits its forward cursor with the **last page** of
  the listing only, and the Drive strategy stores `run_started_at -
  sync.overlap_secs` on its last page, so a cursor never runs ahead of
  undiscovered items.
- The implementation plan linked in the Consequences (`specs/sync-window-plan.md`)
  was deleted when the work was complete (project workflow); the task list is in the
  git history of this change.
