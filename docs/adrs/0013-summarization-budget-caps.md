# ADR-0013: Summarization budget caps (weekly and monthly, hard, from a usage ledger)

- Status: Accepted
- Date: 2026-10-05

## Context

Summaries produced by paid APIs cost money, and the cost of a full pass is not
small compared with the intended running budget. A prototype of this tool spent
over $400 per month because everything was re-summarized every week. The current
design already avoids most of that:

- a sync skips summarization when `input_hash` is unchanged (ADR-0005);
- fetching is incremental (ADR-0012).

But nothing bounds the damage when something else goes wrong or changes:

- a profile switch or a prompt version bump changes `input_hash`, so every entry
  that is normalized again becomes `pending`;
- a growing thread is re-summarized as a whole every time it grows;
- an initial backfill is large by nature.

The only guard today is `--max-cost`, which is **per run**. A scheduled job that
runs every day can spend the per-run limit every day. The cost of a run is also
only kept in `runs.stats`, which is written when the run finishes, so a run that is
killed loses its cost record. `summaries.usage` is overwritten on regeneration
(ADR-0005), so it cannot give a period total.

Measured on the real catalog (Slack only, Claude Haiku 4.5 prices, estimated):
one full pass of the entries that are summarized today costs about $4, and the
steady-state weekly increment is under $0.25. A cap of $2 per week and $10 per
month therefore leaves room for normal operation and a one-time backfill, and
stops a runaway.

## Decision

1. **Two hard caps**, kept as settings in the catalog (`settings` table):

   | Key | Default | Meaning |
   |---|---|---|
   | `summary.budget.weekly_usd` | `2.0` | Maximum estimated spend per week. A week starts on Monday 00:00 local time |
   | `summary.budget.monthly_usd` | `10.0` | Maximum estimated spend per month. A month starts on the 1st, 00:00 local time |

   The purpose is to keep AI token cost from growing without notice. The periods
   are an internal accounting convention of this tool, chosen for simplicity. They
   are not meant to match any provider's billing cycle, and the figures are not
   invoices.

   - A migration writes both rows (`INSERT OR IGNORE`), so existing homes get the
     defaults as visible settings, and `sb config list` shows them as set values.
     The same values are the in-code fallback if a row is removed with
     `sb config unset`.
   - A value of `0` or `null` disables that cap. Disabling is an explicit
     `sb config set`, never a side effect.
2. **A usage ledger** records every paid LLM call: a new table `llm_usage`, one row
   per summarization attempt, with tokens, estimated cost, model and profile. The
   row is written in the same transaction as the summary it paid for. Attempts that
   fail after the provider has billed them (invalid JSON that was retried, a
   map-reduce chunk) are recorded too. The ledger is append-only.
3. **Enforcement before each call.** The summarize stage computes
   `remaining = min(weekly cap − week spend, monthly cap − month spend)`. A unit is
   started only if its estimated cost still fits in `remaining` after the cost of
   calls already in flight. Otherwise the stage stops cleanly, as it does for the
   other limits (ADR-0012): the run ends `stopped_by_limit`, in-flight calls finish,
   unprocessed entries stay `pending` and do not consume attempts.
   - `--max-cost` stays and is applied in addition; the stricter limit wins.
   - There is no flag that bypasses the budget. To spend more, raise the setting,
     which is a deliberate and recorded act.
4. **Fail closed when the cost is unknown.** If a profile of kind `llm_api` has no
   known price (not in the built-in table and not in `llm.prices`) while a cap is
   enabled, it is not run. An issue `llm.unpriced` is opened. A cap that cannot be
   measured is not a cap.
5. **Which spend counts.** Calls of profiles of kind `llm_api`, and of `llm_cli`
   when the CLI reports a cost. Calls to `local_llm` profiles are recorded with a
   cost of 0 and are never blocked by the budget.
6. **Visibility.** `sb stats` shows spend against both caps. A stop caused by a cap
   opens the issue `llm.budget_exhausted` (warning), which `doctor` and the
   notification sinks (ADR-0011) report, and which is resolved automatically by the
   first summary that runs after the period rolls over or the cap is raised.
7. **History.** The budget and the actual spend of past periods can be looked up
   later:
   - the spend of any period is computed from the ledger, never stored, so it
     cannot drift from it;
   - the cap in effect is not recoverable from the ledger (settings change), so a
     small table `budget_periods` keeps one snapshot row per period (week or
     month): its exact boundaries and the cap at the last time the tool evaluated
     it, and when a cap first stopped a run in that period;
   - a new read-only command `sb budget` shows the current periods, the history
     (weeks and months, with cap, spent, share used and whether a cap stopped a
     run), the total since the ledger began, and a breakdown by model. `--json`
     gives the same data.
8. **Minimum size.** Separately from the budget, the existing thresholds
   `summary.min_chars` and `summary.min_messages` are applied again when the
   summarize stage selects entries, not only when an entry is normalized (see
   [summarization.md](../specs/summarization.md)). Entries below the thresholds
   are marked `skipped` without an LLM call. Raising `summary.min_chars` then takes
   effect for entries that are already `pending`.

## Consequences

- The worst case of a month is bounded by the monthly cap plus the in-flight
  calls at the moment the cap is reached (at most `concurrency` calls). Costs are
  estimates from token counts and list prices. The provider's invoice can differ.
- A deliberate large job (an initial backfill of about $4 to $7) spreads over
  several weeks under a $2 weekly cap, and resumes by itself because pending work
  is kept (ADR-0012). The user can raise the weekly cap for a one-off job.
- The schema gains the tables `llm_usage` and `budget_periods` (migration
  `0002`). No existing table changes. Past spend is not reconstructed: runs so far used a local model that
  costs nothing.
- A provider-side spend limit (for example in the provider's console) is still
  recommended as an independent second line of defence. This ADR does not
  replace it.
- History starts when the ledger starts. Spend before that is not reconstructed;
  it was spent on a local model and cost nothing. If a cap is changed during a
  period, the snapshot shows the value at the last evaluation, not every change.
- Weekly spend can be unused and does not carry over. The monthly cap is the real
  ceiling.

## Alternatives considered

- **Sum `runs.stats` instead of a ledger.** No schema change, but a run killed by
  the OS records nothing, failed-but-billed attempts are not attributed, and
  per-model or per-profile reporting is impossible. A ledger written in the
  summary's own transaction is exact.
- **Rolling windows (last 7 / 30 days).** No cliff at the period boundary, but the
  remaining budget is harder to explain, and a period cannot be named in a history
  ("week of 2026-10-05"). Calendar-style periods are easier to reason about and to
  look up.
- **Store the spent amount in the period rows.** A second copy of the ledger that
  can disagree with it. Spent is always summed from the ledger.
- **Store every cap change as an event.** Exact, but heavier than the question
  needs ("what was the budget that week, and what did I spend"). A snapshot per
  period answers it.
- **A bypass flag such as `--ignore-budget`.** Convenient for one-off jobs, and
  exactly what defeats a guard in an unattended or mistaken run. Raising the
  setting is almost as easy and leaves a trace.
- **Warn only, never stop.** A warning nobody reads does not stop a runaway.
- **Keep only the per-run `--max-cost`.** This is the status quo and does not bound
  repeated runs.
- **Make the budget part of the profile.** Spend is a property of the whole home,
  not of one profile.

## Amendments

### 2026-10-05: details settled during implementation

- A third setting, `summary.budget.timezone` (default: the OS time zone), defines
  "local time" for the week and month boundaries, so that tests and users can pin
  it. The boundaries are stored with each period (`budget_periods`), so a later
  change does not move past periods.
- The ledger has a `calls` column (LLM calls per attempt), which the report needs.
- A single unit whose estimate exceeds a whole period's cap is skipped and left
  `pending` with a warning instead of stopping the stage; otherwise one huge entry
  would block every later run.
- The "fail closed when the cost is unknown" rule applies to profiles of kind
  `llm_api`. A `llm_cli` profile reports its own cost; a call without a reported
  cost is recorded with an unknown cost and counted as "unpriced".
- A stored cap that is not a number is treated as the default, never as "off".
  Only `0` or `null` disable a cap.
