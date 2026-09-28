# ADR-0012: Resumable, interruptible and incremental ingestion

- Status: Accepted
- Date: 2026-09-29

## Context

The first sync of an account can take hours. It back-fills a year of Slack and
months of meetings, and with an external LLM it also costs real money. Users must be
able to stop it at any time (Ctrl+C, closing the terminal, the scheduler stopping
the task, a crash) without losing work that was already done or paid for. Running
the command again must continue where it stopped, not start over.

Slack threads can grow very long, and replies keep arriving on old threads.
Re-downloading a whole thread for every new reply wastes API quota and time.

## Decision

### 1. Every unit of work commits on its own

- **Fetch:**
  - An entry's raw files are written to `tmp/` and atomically renamed into
    `raw/`. Only then does the SQLite transaction commit, upserting the entry, its
    raw object rows and the cursor that covers it.
  - Commits are made in **chunks** of up to `pipeline.commit_batch` entries
    (default 20), or at the end of a source page, whichever comes first.
  - An interruption can lose at most one uncommitted chunk, which is fetched again
    on the next run.
- **Summaries are committed one entry at a time**, as soon as each LLM call
  returns. A paid summary is never discarded because a later one failed or the run
  was stopped.
- SQLite runs in WAL mode, so a killed process leaves the database consistent at
  the last committed transaction.
- A raw file renamed into place but not yet committed is an orphan. The next fetch
  of the same entry overwrites it (paths are deterministic), and `sb doctor --fix`
  removes leftovers.

### 2. Cursors never run ahead of committed data

- Source cursors in `sync_state` are updated **in the same transaction** as the
  entries they cover. After any interruption, a cursor points at or before the
  first unprocessed item.
- Work discovered but not yet done is persisted in a **work queue** (`sync_queue`)
  in the same transaction as the cursor that discovered it. Examples are threads
  found while scanning channel history, or Calendar events with note attachments.
  A resumed run drains the queue first, then continues discovery from the
  cursors.
- Back-fills proceed in bounded, chronological **windows** (default 7 days per
  conversation). A window is fully committed before the cursor moves past it, so
  an interrupted back-fill repeats at most one window.

### 3. Re-runs skip finished work

| Stage | "Already done" means |
|---|---|
| Fetch | The entry exists and its fetch state (e.g. last message ts) is at or beyond what the source reports |
| Normalize | `raw_hash` is unchanged since the last normalization |
| Summarize | `summary_status = done` and the stored `input_hash` equals the current one |
| Re-summarize | The current summary already has the target generator (profile's provider + model + prompt version) and the same `input_hash`. Such entries are skipped unless `--force` is given, so an interrupted `sb resummarize` resumes by re-running the same command |
| Import | Upsert by natural key (already idempotent) |

- Failed summaries are retried on later runs up to `summary.max_attempts`
  (default 3), so a permanently failing entry does not burn money every night.
  `sb summarize --retry-failed` resets the counter.

### 4. Graceful stop

- **First** Ctrl+C / SIGINT / SIGTERM (or Ctrl+Break / console close on Windows):
  1. Stop starting new work.
  2. Let in-flight fetches and LLM calls finish and commit, waiting at most
     `pipeline.shutdown_grace_secs` (default 30). LLM calls in flight are already
     paid for.
  3. Mark the run `interrupted`, release the lock, print a resume hint, and exit
     with code 130.
- **Second** signal: abort immediately. In-flight work is lost, and committed work
  is intact.
- Limits stop a run cleanly in the same way:
  - `--max-summaries N`;
  - `--max-cost USD`: cumulative cost estimated from token usage and the price
    table;
  - `--time-limit DURATION`.

  A run stopped by a limit is marked `stopped_by_limit` and exits with code 0.
- On interruption and at the end of a run, a summary is printed: how many entries
  were committed, how many summaries were done and what they are estimated to have
  cost, and what is still pending (queued fetches, pending summaries).

### 5. Incremental raw data for growing sources

- A raw object has a **segment number** (`seq`). Sources whose items grow by
  appending may store a new segment containing **only the new part** instead of
  replacing the raw data. Examples are Slack threads and channel-days, and later
  mail threads. Existing segments are never modified.
- Each entry keeps a source-defined `fetch_state`, for example
  `{"last_ts": "1727…"}` for a Slack thread. The source uses it to ask for only
  newer items. For Slack, this is `conversations.replies` with `oldest = last_ts`.
- `normalize` reads all segments in order and de-duplicates by the item's own ID
  (Slack `ts`), so overlaps are harmless.
- Summaries are still per entry. When a thread grows, the whole thread is
  re-summarized (its `input_hash` changed). Fetching is incremental; summarizing
  is not.
- A **full refetch** (`sb refetch --entry …`, or a source that cannot append)
  replaces all segments with a single new segment 0. This is also how
  edits/deletions of older messages can be picked up on demand.
- Sources whose items are rewritten rather than appended (e.g. a Google Doc) always
  replace: they write segment 0 only.

## Consequences

- Stopping a long first import costs at most one chunk of fetches and nothing
  already summarized. Re-running the same command continues.
- The schema gains `raw_objects.seq`, `entries.fetch_state`,
  `entries.summary_attempts`, the `sync_queue` table and new run statuses. See
  [specs/data-model.md](../specs/data-model.md).
- Every source adapter must be written to these rules. The source test suite
  includes an "interrupt after N commits, resume, compare with an uninterrupted
  run" test per source.
- Long threads accumulate many small segments. `normalize` handles any number of
  them. A later `sb raw compact` could merge segments, but it is not needed for
  correctness.
