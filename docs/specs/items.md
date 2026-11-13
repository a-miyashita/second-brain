# Items, recent entries and reconciliation

Related ADRs: 0002, 0005, 0013, 0016, 0017, 0024.

Items are structured conclusions extracted from an entry in the same LLM call as its
summary: decisions, actions, resolved matters, open questions and accomplishments.
They answer questions that have no keyword ("what are my TODOs today?", "what did I do
this year?"). Rationale and rejected options: ADR-0024.

## Item kinds

| Kind | Meaning | Own fields | `status` |
|---|---|---|---|
| `decision` | Something agreed or decided | `actors` = who decided | — |
| `action` | Work that someone committed to or was asked to do | `owner`, `requester`, `due` | `open`, `done`, `dropped` |
| `resolved` | A problem, blocker or issue that was closed, with no action of its own in the entry | `actors` = who closed it | — |
| `question` | A question left without an answer in the entry | `owner` = who is expected to answer, if said | `open`, `answered` |
| `accomplishment` | Work reported as done that was **not** tracked as an action in the entry | `actors` = who did it | — |

Rules that keep the kinds apart (they are also in the prompts):

- **One fact, one item.** A task that is promised and reported done in the same entry is
  **one** `action` with `status = done`, not an action plus an accomplishment.
- A question that is answered inside the entry is not a `question`: it is a `decision` or
  a `resolved` item.
- `dropped` needs an explicit cancellation or refusal in the input. A silent task stays
  `open`.
- Nothing is inferred from silence, and nothing outside the input is added.

## Common fields

| Field | Meaning |
|---|---|
| `text` | One sentence, in the output language (`summary.language`) |
| `actors` | Raw references of the people involved, as the input writes them (names). Empty if unknown |
| `confidence` | `high`: stated explicitly. `medium`: clear from the context. `low`: inferred. Weaker than `low` is not emitted |
| `evidence` | The shortest verbatim quote (at most 200 characters, in the language of the input, **not** translated) that supports the item. For a status change, the quote that shows the change |
| `occurred_at` | When it was said or happened (below) |
| `scope_key` | Where related items live (below). Computed by the pipeline, never by the model |

Provenance is not repeated per item. The items of an entry come from the same call as its
`summaries` row, so the generator, model, profile and prompt version are those of the
summary (ADR-0005). The JSON of a query joins them in as `extracted_by`.

### Output of the v2 prompts

```json
{"overview": "2-4 sentences",
 "items": [
   {"kind": "action", "text": "Dana updates the CSV spec", "owner": "Dana",
    "requester": "Kei", "due": "2026-11-20", "status": "open",
    "actors": ["Dana", "Kei"], "confidence": "high",
    "evidence": "Dana: I'll update the CSV spec by next Friday"},
   {"kind": "decision", "text": "The export format is CSV", "actors": ["Kei"],
    "confidence": "high", "evidence": "OK, CSV it is"}
 ],
 "details": "meeting prompt only"}
```

- `items` is required and may be empty (chit-chat, thin documents). A missing `items` is
  an invalid output and goes through the one repair attempt (summarization.md). Old keys
  (`decisions`, `action_items`) are ignored.
- `due` is an absolute `YYYY-MM-DD`, resolved by the model against the entry date that
  the prompt header gives. If it cannot be resolved without guessing, `due` is `null` and
  the relative phrase stays in `text`.
- At most `items.max_per_entry` (default 30) items are kept, highest confidence first.
- All input, as before, is untrusted data in delimiters. Instructions inside it are
  ignored. Item text copied from it is still data.

### Account owner hint

The prompt gets the names of the user (the union of all account aliases, names only, at
most 8, see [accounts-and-auth.md](accounts-and-auth.md#self-aliases)) as the optional
labeled input `account_owner`. Rules:

- use it to recognize the account owner among speakers and mentions, including first
  person statements in a conversation with several speakers;
- write `owner` and `actors` exactly as the **input** writes the name, never as the
  alias list does;
- never write "me", "the user" or "the account owner" as a person.

Like the other hints (ADR-0023), it is not part of `input_hash`.

### Map-reduce

For an input longer than `max_input_chars` (summarization.md) each chunk call returns
items; the merge call receives the partial items with the partial summaries and removes
duplicates (same kind, same people, same meaning) and keeps the **latest status** of an
action that appears in several chunks. Evidence is verified against the **whole body**
after the merge, never against a chunk.

## Validation (deterministic, in `sb-pipeline`, before the commit)

For every item returned by the model:

1. Normalize the quote and the body the same way: NFKC, lowercase, runs of whitespace
   and Markdown emphasis marks (`*`, `_`, `` ` ``, `~`) collapsed. The quote must be a
   substring of the body.
2. **Verified**: keep the item as returned.
3. **Not verified**:
   - an `action` with `status = done` or `dropped` becomes `open`, confidence `low`;
   - a `resolved` or `accomplishment` item is dropped;
   - an `answered` question becomes `open`, confidence `low`;
   - any other item is kept with confidence `low`.
4. An item without a quote is treated as not verified.
5. `due` that is not a valid date becomes `null`. An unknown `kind` or `status` drops the
   item. `confidence` outside the three values becomes `low`.
6. Empty `text` drops the item.

Counts (`kept`, `downgraded`, `dropped`) are stored in `summaries.usage` under `items`, so
the quality of a model and a prompt can be compared without keeping the dropped text.

### `occurred_at`

Deterministic, with a fallback chain:

- **Slack:** the time of the message that contains the quote. The body is the rendered
  conversation (`HH:MM Name: body`, with date headers when it spans days), so the line of
  the quote gives the clock time, and the nearest date header above it, or the entry date,
  gives the day, in `slack.day_timezone`. If the quote is not found: `source_updated_at`.
- **Meet:** `source_created_at` (the meeting start).
- **Documents:** `source_updated_at`, else `source_created_at`.
- Always a UTC RFC 3339 instant in the store.

### `scope_key`

| Source | `scope_key` |
|---|---|
| `slack.thread`, `slack.day` | `slack:<channel_id>` |
| `google.meet` | `meet:<recurring_event_id>` when the entry has one (below), else `meet:title:<normalized title>` |
| other | `<source_kind>:<source_id>` |

The normalized meeting title is NFKC, lowercase, with the "(recurring)" suffix and a
trailing date removed and whitespace collapsed. Meet entries fetched before the series key
was stored use the title form until `sb refetch` fills `recurring_event_id`.

## Storage

Migration `0005` (details in [data-model.md](data-model.md#items)): `items` and
`item_links`. Items are written inside `commit_summary` in the same transaction as the
sections and the `summaries` row; the transaction first deletes the entry's old items.
The `decisions` and `action_items` sections are rendered from the items (below) and
indexed as before.

An entry whose current summary is `source_native` has no items: committing a native
summary deletes them. An entry that waits for re-summarization (`pending`) keeps its old
items together with its old summary, like its old sections (data-model.md invariant 3).

### Rendering the sections

- `decisions`: one bullet per `decision` item, its `text`.
- `action_items`: one bullet per `action` item. The text is prefixed with `[done] ` or
  `[dropped] ` for those statuses and is plain for `open`.
- `resolved`, `question` and `accomplishment` items are **not** sections. They are
  reachable through `sb items` and `sb recent`.
- An empty list produces no section, as before.

## `sb recent`

```text
sb recent [--since D] [--until D] [--source K].. [--account A].. [--section K].. [--with-items]
          [--limit N] [--json]
```

Entries by **activity time**, newest first, without a keyword. For "what happened today"
and as the fallback for entries without items.

- Activity time is `COALESCE(source_updated_at, source_created_at, ingested_at)`. A thread
  that got a reply today appears today.
- `--since` defaults to `1d`. `D` is `YYYY-MM-DD`, an RFC 3339 instant, an age (`1d`,
  `12w`) or `today` (local midnight in the OS time zone). `--until` is exclusive.
- `--section` is repeatable; the default is `overview`, `decisions`, `action_items`.
- `--with-items` adds the entry's items (the shape of `sb items`).
- `--limit` defaults to 30, at most 200.
- An entry whose summary is not `done` (below the thresholds, pending or failed) has no
  summary. It is still listed, with `summarized: false` and a `details_excerpt` of the
  first 400 characters of its `details`, so a short thread is not invisible.

`--json`, schema `sb.recent/v1`:

```json
{"schema": "sb.recent/v1",
 "query": {"since": "2026-11-13T00:00:00+09:00", "until": null, "limit": 30},
 "entries": [{
   "entry_uid": "01J9...", "title": "#dev Release checklist", "source_kind": "slack.thread",
   "account": {"id": "work-slack", "label": "Work"},
   "activity_at": "2026-11-13T02:41:00Z", "cite_url": "https://...",
   "summarized": true, "items_available": true,
   "sections": {"overview": "...", "decisions": ["..."], "action_items": ["..."]},
   "items": []}],
 "total": 12, "truncated": false}
```

`items_available` is false when the entry has no items (an older prompt version, a native
summary, or no summary). `sections` holds only the requested sections that exist.

## `sb items`

```text
sb items [--kind K].. [--status S].. [--mine | --owner NAME | --asked-by-me]
         [--since D] [--until D] [--due-by D] [--account A].. [--source K].. [--scope KEY]
         [--min-confidence high|medium|low] [--stale | --not-stale]
         [--candidates] [--limit N] [--json]
```

| Option | Meaning |
|---|---|
| `--kind` | `decision`, `action`, `resolved`, `question`, `accomplishment`. Default: all |
| `--status` | Matches the **effective** status (below): `open`, `done`, `dropped`, `answered`, `superseded`. Default: any |
| `--mine` | The owner matches an alias of the user; for kinds without an owner, an actor does. See [accounts-and-auth.md](accounts-and-auth.md#self-aliases). Exit with code 64 and error `aliases.empty` when no alias is known |
| `--owner NAME` | The owner matches this name (same normalization) |
| `--asked-by-me` | The `requester` matches an alias: what the user is waiting for |
| `--since`, `--until` | On `occurred_at`. To list everything that is still open, omit `--since` |
| `--due-by D` | Actions with `due` on or before D. Overdue ones included |
| `--min-confidence` | Default `items.min_confidence` (`medium`). Lower-confidence items are counted in `hidden_low_confidence` |
| `--stale`, `--not-stale` | Only (or without) open items older than `items.stale_days` |
| `--candidates` | Add candidate resolutions to open actions and questions (below) |
| `--limit` | Default 50, at most 200. Order: actions by `due` (nulls last) then newest; others newest first |

The answer to "what are my TODOs today?" is
`sb items --kind action --status open --mine [--due-by <today>] --json`.
The answer to "what did I do in the last six months?" is
`sb items --kind accomplishment --kind resolved --kind action --status done --mine --since 180d --json`.

`--json`, schema `sb.items/v1`:

```json
{"schema": "sb.items/v1",
 "query": {"kinds": ["action"], "status": ["open"], "mine": true, "min_confidence": "medium"},
 "items": [{
   "item_ref": "01J9...#3", "kind": "action", "text": "Dana updates the CSV spec",
   "status": "open", "effective_status": "open", "stale": false,
   "owner": "Dana", "requester": "Kei", "due": "2026-11-20", "actors": ["Dana", "Kei"],
   "mine": {"match": "owner", "basis": "name"},
   "confidence": "high", "evidence": "Dana: I'll update the CSV spec by next Friday",
   "occurred_at": "2026-11-12T05:10:00Z", "scope_key": "slack:C0123",
   "entry": {"entry_uid": "01J9...", "title": "#dev CSV export", "source_kind": "slack.thread",
             "account": {"id": "work-slack", "label": "Work"}, "cite_url": "https://..."},
   "extracted_by": {"model": "claude-haiku-4-5-20251001", "profile": "fast",
                    "prompt_version": "conversation-summary/v2"},
   "resolved_by": [],
   "candidates": []}],
 "total": 1, "hidden_low_confidence": 2,
 "coverage": {"entries_in_range": 40, "entries_with_items": 22,
              "entries_without_items": 18, "unsummarized": 7, "predate_items": 11, "native": 0}}
```

- `item_ref` is `<entry_uid>#<ordinal>`. It is valid until the entry is re-summarized.
- `mine` is `null` when not asked or not matching. `match` is `owner`, `requester` or
  `actor`. `basis` is `email` or `name`.
- `coverage` counts entries in the same account, source and period filters, so an agent
  can say how complete the answer is: `unsummarized` (below the thresholds, pending, or
  failed), `predate_items` (summarized by an older prompt version), `native` (Gemini's own
  notes) and `entries_without_items` (the sum of those three). The range is the
  `--since`/`--until` window on the entries' activity time. The agent never says
  "nothing" when these are large; it says what is missing and gives the command
  (`sb resummarize --estimate`) for the user to run (agent-integration.md).
- `resolved_by` lists items that close this one through a stored link (empty until the
  reconcile stage exists).

### Effective status

For each item:

1. Start with its own `status`. Kinds without a status have none.
2. If a `resolves` link points to it from an item that is later in time, with
   `confidence >= items.link_min_confidence` (default `medium`): an `action` becomes
   `done` and a `question` becomes `answered`. A `supersedes` link makes it `superseded`.
3. `stale` is true when the effective status is `open` and the newest `occurred_at` among
   items of the same `scope_key` is older than `items.stale_days` (default 30) at the
   injected clock's now. `stale` never changes the status and nothing is closed because of
   it. `sb items --stale` lists them for review.

`dropped`, `done`, `answered` and `superseded` are final in this computation.

### Candidate pairs (no LLM)

With `--candidates`, each result item that is an effective-`open` action or question gets
up to 3 `candidates`: items that might close it. A candidate `J` of item `I` must satisfy
all of:

- `J` is closure-like: a `resolved` or `accomplishment` item, a `decision`, or an `action`
  with status `done` or `dropped`;
- `J` is from another entry (the same entry was already judged by the model);
- `J.occurred_at` is after `I.occurred_at` and within `items.candidate_days` (default 60);
- `J` shares the `scope_key` of `I`, **or** shares a person with it (an owner or actor
  equal after name normalization);
- the text similarity is at least `items.candidate_min_similarity` (default 0.2). It is the
  Jaccard similarity of character trigrams of the NFKC-lowercased texts, which also works
  for Japanese.

```json
"candidates": [{"item_ref": "01JA...#1", "kind": "accomplishment", "text": "...",
                "evidence": "...", "occurred_at": "...", "similarity": 0.41,
                "reason": ["same_scope"], "entry": {"entry_uid": "01JA...", "cite_url": "..."}}]
```

A candidate is a lead, not a fact. The agent opens the cited entries, decides, and cites
both. When unsure it says so; it does not state that an item is done.

## Settings

Code defaults, shown by `sb config list`. None needs a migration row.

| Key | Default | Meaning |
|---|---|---|
| `items.min_confidence` | `medium` | Default floor for `sb items` |
| `items.max_per_entry` | `30` | Items kept per entry |
| `items.stale_days` | `30` | Age after which an open item without later activity in its scope is `stale` |
| `items.candidate_days` | `60` | Window of candidate pairs and of the reconcile stage |
| `items.candidate_min_similarity` | `0.2` | Trigram similarity floor of candidate pairs |
| `items.link_min_confidence` | `medium` | Floor of a stored link when computing effective status |
| `summary.profile.reconcile` | unset | Profile of the reconcile stage; falls back to `summary.profile.default` |

## Backfill

New prompt versions make older entries eligible for `sb resummarize` (summarization.md).
Sync itself re-summarizes only entries whose body changed. To get items for the past:

```sh
sb resummarize --source slack.thread --since 180d --profile <P> --estimate
sb resummarize --source google.meet  --since 180d --profile <P> --estimate
```

Without `--force`, an entry already at the target prompt version is skipped, so an
interrupted run resumes. `sb doctor` shows `items.coverage`.

## Reconcile stage (later phase)

Built when `sb items --candidates` shows that agent-side judging is too weak or too slow.
Designed here so that the schema is ready (migration `0005`) and the rules are fixed.

**Trigger.** The stage runs after the summarize stage of `sb sync`, and as
`sb reconcile [--since D] [--entry UID].. [--estimate] [--dry-run] [--max-cost USD]
[--time-limit DUR]`. It processes entries in the table `reconcile_queue` (migration
`0006`, with `llm_usage.purpose`, below).

**Queue.** `reconcile_queue(entry_id PK, reason, enqueued_at)`. An entry is queued when
its items are committed. When an entry is re-summarized, the commit transaction reads the
entries at the other end of the links that cascade away, and queues them too.

**Candidates.** The same rules as the query-time candidate pairs, applied in both
directions for the new entry's items: each new closure-like item against older open items,
and each new open item against **newer** closure-like items. The second direction is what
makes backfill order-independent.

**Judgment.** One call per entry with at least one candidate pair. The input is only the
pairs: for each, both item texts, both evidence quotes, both dates and the shared scope.
No raw body is sent. Prompt `item-reconcile/v1`. Output:

```json
{"links": [{"from": "01JA...#1", "to": "01J9...#3", "relation": "resolves",
            "confidence": "high", "evidence": "<quote from the from-item>"}]}
```

**Validation.** `from` must be later than `to`; `resolves` needs a closure-like `from` and
an open action or question as `to`; `evidence` must occur in the `from` item's text or
quote. Anything else is dropped. The stage writes `resolves` and `supersedes`. `same_as` is
reserved and not written.

**Cost control.** It uses the budget gate and writes to `llm_usage` in the same
transaction as the links. Migration `0006` adds `llm_usage.purpose TEXT NOT NULL DEFAULT
'summary'` so that the ledger can tell the two apart; `sb budget --by-purpose` is optional.
The profile is `summary.profile.reconcile`, else `summary.profile.default`.

**Rebuild.** `item_links` can be deleted and rebuilt from `items`: `sb reconcile --rebuild`
queues every entry with items. Links carry `linked_by_model` and `prompt_version`.
