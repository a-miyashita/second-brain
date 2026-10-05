# Summarization

Related ADRs: 0005, 0013.

## Output structure

Summarizers must return JSON matching:

```json
{"overview": "2-4 sentences",
 "decisions": ["who decided what"],
 "action_items": ["who does what by when"],
 "details": "optional; only for sources whose details are generated (meetings)"}
```

These are rendered to Markdown sections with `origin = generated`:

- `overview` as text;
- `decisions` and `action_items` as bullet lists; empty arrays produce no section;
- `details` as text, only when requested for the source kind.

Invalid JSON is retried once with a repair instruction. If it fails again, the entry
gets `summary_status = failed` and an `llm.bad_output` issue.

## Commit and resume rules (ADR-0012)

- Each summary is committed in its own transaction as soon as it returns. Paid work
  is never lost to a later failure or an interruption.
- Selection for `sb summarize` and the summarize stage of `sb sync` is
  `summary_status IN (pending, failed) AND summary_attempts < summary.max_attempts`
  (default 3). `--retry-failed` resets the attempt counters of the selected
  entries.
- **Limits:** `--max-summaries N`, `--max-cost USD` (estimated from actual token
  usage and the price table) and `--time-limit`. When a limit is reached, no new
  calls are started, in-flight calls finish, and the run ends as
  `stopped_by_limit`. In addition, the weekly and monthly **budget** below always
  applies; the stricter of all limits wins.
- **Progress:** during a run, stderr shows done / remaining and the running cost
  estimate. The final line says what remains and how to continue.

## Prompt rules

- Write only what the input says. No speculation.
- Keep proper nouns, numbers and dates verbatim.
- Decisions say who decided what. Action items say who, what and by when; the owner
  is omitted if unknown.
- Chit-chat gets a one-line overview and empty lists.
- Output language: the setting `summary.language`: `auto` (same as the input,
  default), `ja` or `en`.

Prompts are embedded per purpose, each with a version:

| Prompt | Used for |
|---|---|
| `conversation-summary/v1` | slack.* |
| `meeting-summary/v1` | google.meet from transcript. Also produces `details` |
| `document-summary/v1` | google.doc, web.page, local.file, mail.* |

## Input construction

The source adapter produces a `SummaryInput`:

| Source kind | Input body |
|---|---|
| slack.* | The formatted conversation (the same text as the `extracted` details section) |
| google.meet | The transcript if present (embedded in the notes or a separate document; see source-google-meet.md), after mechanical cleanup. Otherwise the Gemini notes document text (the second case re-summarizes Gemini's own summary; allowed, but `doctor` shows how many meet entries lack transcripts) |
| documents | Extracted text; the user context is passed separately as a hint |

- `input_hash` is SHA-256 over the prompt version, the profile's model and the body.
  A sync skips summarization when the stored `input_hash` matches.
- **Long inputs:** if the body exceeds the profile's `max_input_chars` (default
  40 000, tunable per profile), it is summarized map-reduce style:
  1. split into chunks on message or paragraph boundaries;
  2. produce partial summaries;
  3. make a final merge call.

  Head+tail truncation is not used, because transcripts are routinely longer than
  the limit and the middle would be lost.
- **Thresholds:** conversations shorter than `summary.min_chars` (default 400) or
  `summary.min_messages` (default 3) get `summary_status = skipped`. Only the
  extracted details are kept.
  - The thresholds are checked **twice**: when an entry is normalized, and again
    when the summarize stage processes the entry. The second check rebuilds the
    input from raw data (the same step `resummarize` uses) and marks an entry below
    the thresholds `skipped` **without an LLM call**. This is what makes a raised
    `summary.min_chars` apply to entries that are already `pending`. (`resummarize`
    does not apply the thresholds: it was asked for explicitly.)
  - It never touches an entry that is already `done`: an existing summary is kept
    even if it is now below the threshold.
  - Lowering a threshold does not revive `skipped` entries by itself. Run
    `sb reextract` on them to evaluate them again.
  - Suggested value when summaries come from a paid API: `summary.min_chars = 2000`.
    Short threads rarely gain from a summary (their text is already short and is
    indexed as `details`), and they are many (measured on one real catalog: about two thirds of the
    entries above 400 characters are below 2000 and account for about 58% of the
    estimated cost of a full pass, while they carry the least information per call).

## Budget (ADR-0013)

Spend on paid summarization is capped per calendar week and per calendar month,
independently of any per-run limit.

| Setting | Default | |
|---|---|---|
| `summary.budget.weekly_usd` | `2.0` | A week starts on Monday 00:00 local time |
| `summary.budget.monthly_usd` | `10.0` | A month starts on the 1st, 00:00 local time |
| `summary.budget.timezone` | OS time zone | IANA name that defines "local time" for both periods |

The periods exist to stop token cost from growing unnoticed. They are an internal
convention and need not match any billing cycle; the amounts are estimates computed
by this tool, not invoices.

- "Local time" is `summary.budget.timezone` (default: the OS time zone, like
  `slack.day_timezone`), applied to the injected `Clock`. Ledger timestamps are UTC;
  period boundaries are converted from local time and stored with the period (see
  History), so a later change of the time zone does not move past periods.
- `sb config set` accepts a non-negative number, `0` or `null` for the two caps and
  an IANA name for the time zone, and rejects anything else. A stored cap that is
  not a number is treated as the default, never as "off".
- Both rows are written to `settings` by migration `0002`, so `sb config list`
  shows them as values, not as defaults. `0` or `null` disables a cap; it is set
  with `sb config set summary.budget.weekly_usd 0`. If a row is removed, the
  in-code default above applies.
- The cap applies to every command that summarizes: the summarize stage of
  `sb sync`, `sb summarize`, `sb resummarize` and `ingest`.

### Ledger

Each summarization attempt appends one row to `llm_usage` (see data-model.md):

- the row is written in the **same transaction** as the summary it paid for, with
  the id of the `sync` run when there is one (`sb summarize` and `sb resummarize`
  do not create run records, so their rows have no run id);
- an attempt that fails after the provider billed it (output that is invalid twice,
  a map-reduce chunk, a merge call) is also recorded. The summarizer therefore
  returns the usage consumed so far in its error, not only in its success value;
- `cost_usd` is the provider-reported cost when present (`claude_cli`), else
  tokens times price; it is NULL when no price is known;
- `local_llm` calls are recorded with cost `0`;
- an attempt that is billed but fails is recorded with `outcome = failed`.

### Enforcement

Before starting each summary unit, the summarize stage computes:

```text
spent_week  = SUM(cost_usd) over the current calendar week
spent_month = SUM(cost_usd) over the current calendar month
remaining   = min(weekly_cap - spent_week, monthly_cap - spent_month)   // disabled caps are ignored
reserved    = estimated cost of calls already in flight in this run
needed      = estimated cost of this unit
```

The unit starts only if `needed + reserved <= remaining` and something is left at
all (an estimate of zero, for a CLI whose price is unknown, still cannot start on an
exhausted budget). Profiles of kind `local_llm` are never gated. The estimate is
`estimate_tokens(input) + system prompt` input tokens and `ESTIMATED_OUTPUT_TOKENS`
output tokens at the profile's price (the same estimate as `--estimate`).

When it does not fit:

- no new calls are started; in-flight calls finish and are recorded;
- the run ends `stopped_by_limit` with the detail `budget.weekly` or
  `budget.monthly` (whichever was binding), and the exit code is 0;
- entries not processed stay `pending`, and `summary_attempts` is not incremented;
- the issue `llm.budget_exhausted` (warning) is opened, with the spend, the cap and
  the date the cap resets. It is resolved by the first summary that runs after the
  period rolls over or the cap is raised.

A single unit whose own estimate is larger than a whole period's cap can never run
under this budget. It is skipped (left `pending`, counted as deferred) with a
warning, and the stage carries on with the next entry; it does not stop the stage,
otherwise one huge entry would block every later run.

Because the estimate can be off, the real spend of a period can exceed a cap by at
most the unreserved error of the calls in flight (at most `concurrency` calls).

If a profile of kind `llm_api` has no known price while any cap is enabled, the
profile is not run (a `llm_cli` profile is not affected: its CLI reports the cost,
and a call without a reported cost is counted as "unpriced"): the issue `llm.unpriced` (error) is opened and its entries stay
`pending`. Set `llm.prices` for the model, or disable the caps explicitly.

### History

Each time the summarize stage evaluates the budget it upserts the rows of the
current week and month in `budget_periods` (boundaries, cap in effect), and it sets
`stopped_at` when a cap stops a run. Together with the ledger this answers, for any
past week or month, what the budget was and what was spent.

- Spend is summed from `llm_usage`; it is not stored in `budget_periods`.
- A period's cap is the value at the last evaluation. Changing a cap in the middle
  of a period updates the row; earlier values are not kept.
- A call whose cost is unknown (`cost_usd` NULL) is not added to the sum and is
  counted separately as "unpriced calls".
- History starts with the ledger. Nothing is reconstructed for earlier runs.

### Reporting

- `sb budget` is the full report (below). `sb stats` prints only the short form,
  spend against both caps and the reset dates:

  ```text
  Summarization budget (estimated):
    this week   $0.42 / $2.00   resets 2026-10-12 (Mon)
    this month  $1.10 / $10.00  resets 2026-11-01
  ```

  With `--json`, the object gains a `budget` field (adding a field needs no schema
  version bump):
  `{"weekly": {"spent_usd": 0.42, "cap_usd": 2.0, "resets_at": "..."}, "monthly": {...}}`.
  A disabled cap has `"cap_usd": null`.
#### `sb budget [--weeks N] [--months N] [--by-model] [--json]`

Read-only. Defaults: the last 8 weeks and the last 6 months. Money is shown with
two decimals (four for amounts below $0.01).

```text
Summarization spend (estimated by second-brain, not an invoice)

Current
  week   2026-10-05 .. 2026-10-11   $0.42 / $2.00   21%
  month  2026-10-01 .. 2026-10-31   $1.10 / $10.00  11%
Total since 2026-10-06: $1.10 in 312 calls (0 unpriced)

Weeks
  start       cap     spent   used  calls  stopped
  2026-10-05  $2.00   $0.42    21%     45  -
  2026-09-28  $2.00   $2.00   100%    210  2026-10-02 18:41
  ...
Months
  start       cap      spent   used  calls  stopped
  2026-10-01  $10.00   $1.10    11%    312  -
  ...
By model (with --by-model)
  claude-haiku-4-5   $2.40  288 calls  1.9M in / 0.2M out tokens
  gemma4:e2b         $0.00  920 calls
```

`--json` has `"schema": "sb.budget/v1"` and the fields:

- `current`: `{"week": {...}, "month": {...}}`, each with `period_start`,
  `starts_at`, `ends_at`, `cap_usd` (null = disabled), `spent_usd`, `calls`,
  `unpriced_calls`, `resets_at`;
- `weeks`, `months`: arrays of the same objects for past periods, newest first,
  plus `stopped_at`;
- `total`: `{"spent_usd", "calls", "unpriced_calls", "since"}`;
- `by_model` (always present in JSON): `[{"model", "provider", "spent_usd", "calls",
  "input_tokens", "output_tokens"}]`.

The cap shown for the **current** periods is the cap in force now. The cap shown
for **past** periods is the one stored in `budget_periods` (the value at the last
evaluation in that period). A period with no row in `budget_periods` is not listed. A period that has a row
but no spend shows `$0.00`.

- `sb summarize --estimate` and `sb sync --estimate` also print the remaining
  budget and whether the estimate fits in it; if not, how many entries fit. The
  JSON estimate gains a `budget` object: `remaining_usd`, `paid_entries`,
  `entries_that_fit`, `fits` (absent when no cap is enabled or nothing is paid).
- `sb doctor` lists the open budget issues (see doctor.md).

## Profiles

Setting `llm.profiles.<name>`:

```toml
[llm.profiles.fast]
kind = "llm_api"            # llm_api | llm_cli | local_llm
provider = "anthropic"      # anthropic | openai | google | openai_compatible | claude_cli | copilot_cli
model = "claude-haiku-4-5"
concurrency = 8
max_input_chars = 40000
secret = "global:anthropic.api_key"   # optional; env fallback per ADR-0003

[llm.profiles.npu]
kind = "local_llm"
provider = "openai_compatible"
base_url = "http://127.0.0.1:5273/v1" # discovered by `sb setup llm` for Foundry Local
model = "phi-4-mini-instruct-openvino-npu"
concurrency = 1
request_timeout_secs = 300
warmup_timeout_secs = 600             # first call may include model load
start_command = ["foundry", "service", "start"]   # optional; run if the server is unreachable
keep_alive = "30m"                    # optional; forwarded where the runtime supports it
```

The profile model strings above are examples only.

Selecting a profile:

- `summary.profile.default = "fast"`
- `summary.profile.<source_kind> = "..."` overrides the default per source kind.
- `native` is a reserved profile name (not definable).
  `summary.profile.google.meet = "native"` means "keep Gemini notes, do not
  summarize". This is the default for google.meet.

### Local LLM servers (`kind = "local_llm"`)

The model is loaded and kept by the server process (Foundry Local, OpenVINO Model
Server, Ollama, llama.cpp server), not by second-brain. Each summary is one HTTP
request. Loading happens at most once per server lifetime, or again after the
runtime's idle unload. It never happens per entry.

Before the first summary of a run, the summarize stage does the following:

1. **Reachability:** `GET <base_url>/models`, with a short timeout (5 s).
   - If the server is unreachable and `start_command` is set, run it, then poll
     the endpoint until `warmup_timeout_secs`.
   - If the server is still unreachable, leave the entries `pending` (they are not
     counted as failed attempts), open an `llm.local_unreachable` issue, and
     continue the run without summaries. Fetched data is unaffected.
2. **Warm-up:** send one tiny completion (a few tokens) with
   `warmup_timeout_secs` (default 600). This absorbs lazy model loading, and the
   elapsed time is logged. Real requests then use `request_timeout_secs`.
3. **Keep-alive:** when `keep_alive` is set and the provider supports it, it is
   sent with every request, so the model is not unloaded between the fetch and
   summarize stages. Ollama supports this with `keep_alive`. Other runtimes may
   ignore it; their own idle-unload setting applies.

If a request times out in the middle of a run (for example, because the runtime
unloaded the model), it is retried once with `warmup_timeout_secs` before it counts
as a failed attempt.

`start_command` is also used by the scheduled job. The job's `PATH` includes the
command's directory (see setup-and-scheduling.md).

### LLM CLI providers

- They run in a fresh empty directory under `$SECOND_BRAIN_HOME/tmp/`, so no
  project `AGENTS.md` / `CLAUDE.md` is picked up.
- The prompt goes on stdin, so there are no argv length limits. A timeout applies
  (default 300 s).
- `claude_cli`: `claude -p --output-format json [--model M]`.
- `copilot_cli`: `copilot -p <prompt>` with the equivalent non-interactive flags.
  The exact flags are verified at implementation time.
- The CLI's login state is checked by `sb doctor --online` with a trivial prompt.

### Foundry Local wizard (`sb setup llm`, phase 2)

1. Detect the `foundry` CLI and the service status (`foundry service status`), and
   discover the endpoint URL.
2. List catalog models, highlighting NPU/OpenVINO variants for the detected
   hardware.
3. Download and load the selected model if needed. The user confirms first, because
   models are large.
4. Run a test summarization with a fixed sample and show the output and latency.
5. Save the profile. Optionally set it as the default for a source kind.

The wizard is a library function in `sb-setup`, so a future GUI can reuse it.

## `sb resummarize`

```text
sb resummarize [--account A] [--source K] [--since D] [--until D] [--entry UID]...
               [--where-model M] [--where-provider P]
               (--profile P | --native)
               [--estimate] [--dry-run] [--limit N] [--yes]
```

1. Select entries by the filters. `--where-model` matches the current
   `summaries.model`. Entries whose current summary already has the target
   generator (provider, model, prompt version) and the same `input_hash` are
   **skipped** unless `--force` is given. That is what makes an interrupted
   re-summarization resumable: run the same command again.
2. Skip entries with `raw_status != present`, and report how many. The suggested
   remedy is `sb refetch` on the same filters.
3. `--estimate`: count tokens (character-based heuristic) and show the estimated
   cost from the per-model price table in the binary. Prices are overridable by the
   setting `llm.prices`.
4. Without `--yes`, ask for confirmation when more than 20 entries are affected.
5. For each entry, rebuild the input from raw data, summarize, then in one
   transaction replace the `generated` sections and the `summaries` row and
   re-index. The limits and graceful-stop rules above apply.
6. `--native` (google.meet only) re-extracts Gemini notes from raw data and records
   `source_native` again.
