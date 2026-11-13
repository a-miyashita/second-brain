# ADR-0024: Structured items, time-based retrieval and cross-entry links

- Status: Accepted
- Date: 2026-11-13

## Context

Search needs a keyword. A question such as "What are my TODOs today?" or "What did I do
over the last six months?" has none: the answer is spread over many Slack threads and
meeting notes, and the words that matter ("I will check it by Friday", "done, merged")
are not the words a search would use. The conclusions exist in the data, but the
catalog cannot return them by kind, by person, by period or by state.

Facts that shape the design:

- A summary already has `decisions` and `action_items` (ADR-0005), but they are plain
  bullet lists. They carry no owner, no due date, no state, no time, and no link to the
  words that support them.
- The entry is already the right unit. A Slack thread is one entry and a grown thread is
  re-summarized as a whole (source-slack.md), so "I will do it" followed by "done" in one
  thread is read in one prompt. A Slack `slack.day` entry cuts a conversation at local
  midnight. A meeting is one entry.
- Summaries are regenerated only when the body changes (ADR-0017), and the input hash
  covers the body alone. A summary must stay a function of its body so that
  regeneration is stable and re-running is safe.
- Ingestion is resumable and can be backfilled (`sb sync --since`, ADR-0016). The entry
  that closes an item can arrive before the entry that opened it.
- Google Meet entries use Gemini's own notes by default (`native`), so no LLM sees the
  transcript. The cost of a Haiku-class call is now low enough to change that.
- Slack renders speakers and mentions as names, not IDs (`HH:MM Name: body`). Gemini
  notes name people in running text.

## Decision

1. **Two additions to retrieval.** `sb recent` returns entries by activity time, with
   summaries, and no keyword. `sb items` returns **structured items** by kind, state,
   person and period. Both have MCP tools. The skill teaches agents to answer a vague or
   time-based question with these, and to use `sb search` for topics.

2. **Items are extracted in the same LLM call as the summary.** The summary output gains
   `items`, a list of typed records: `decision`, `action`, `resolved`, `question`,
   `accomplishment`. Actions have an owner, a requester, a due date and a status
   (`open`, `done`, `dropped`). Every item has a `confidence`, a verbatim `evidence`
   quote and a time. `decisions` and `action_items` sections are **rendered from the
   items**, so search keeps working. The new prompt versions are `conversation-summary/v2`,
   `meeting-summary/v2` and `document-summary/v4`.

3. **Items are generated data, owned by the entry.** They are replaced together with the
   summary in the same transaction, and have no history (ADR-0005). Past data gets items
   only through re-summarization (`sb resummarize`; a new prompt version makes older
   entries eligible). Nothing is extracted separately.

4. **Evidence is checked mechanically.** A quote that does not occur in the input is not
   trusted. A completion claim without verified evidence is never kept as a completion:
   an unverified `done` becomes `open` with low confidence, and an unverified `resolved`
   or `accomplishment` is dropped. A false "open" is a mild error; a false "done" hides
   work.

5. **Links between entries are a separate, derived layer.** `resolves`, `supersedes` and
   `same_as` between items of different entries are stored in `item_links` and are never
   part of the summary output. Open items are **not** put into the summary prompt:
   that would make the output depend on catalog state and ingestion order, break the
   stable regeneration of ADR-0017, and invite the model to find completions that are
   not there. Links are rebuildable data, like the search index (ADR-0002). A
   re-summarization deletes the links of the entry's items by cascade and queues the
   affected entries for reconciliation again.

6. **Reconciliation ships in two steps.**
   - **Now:** `sb items` computes **candidate pairs** without an LLM (same scope or a
     shared person, a later time, within a window, similar text) and returns them next to
     each open item. The agent judges them at query time and cites both entries.
   - **Later (documented here, built after use shows it is needed):** a reconcile stage
     with one small LLM call per new entry over item pairs and their evidence quotes only
     (never the raw bodies), budget-gated by ADR-0013, which writes `item_links`.

   Effective state is computed at query time: an item's own state, overridden by an
   incoming `resolves` link above a confidence threshold. An open item with no later item
   in its scope for a number of days is marked `stale`. **Nothing is closed
   automatically.**

7. **"Mine" is decided at query time.** Owners and actors are stored as raw references,
   as the input wrote them (names). `--mine` compares them with the **aliases** of the
   user's accounts, which are collected automatically (Slack user directory, Meet
   participant lines and Calendar `self` attendees, the Microsoft ID token). The user
   is not asked to type their name. A wrong or missing alias is fixed without
   re-extraction. The account owner's names are also given to the summarizer as a hint,
   so that name-only mentions can be tied to the right speaker.

8. **Google Meet is summarized by an LLM by default.** `summary.profile.google.meet`
   falls back to `summary.profile.default`, and to `native` only when no profile is
   configured. The Meet fetch also stores the Calendar `recurringEventId` as a stable
   series key. `native` stays available per setting and through `sb resummarize
   --native`; a native summary has no items.

9. **Deferred.** `same_as` links across Slack and Meet, aggregation entries for a
   meeting series, manual status overrides, and a first-class `waiting_on` kind. Manual
   overrides need an item identity that survives regeneration, which this design does not
   have: re-summarization is rare and the user accepts that it resets items.

Details are in [items.md](../specs/items.md). The staged work is in
[items-plan.md](../specs/items-plan.md).

## Consequences

- **Schema.** Migration `0005` adds `items` and `item_links`. `item_links` is created
  empty and stays unused until the reconcile stage exists, so the query code that reads
  links is written and tested from the start. `SummaryOutput` gains `items`; the
  `decisions` and `action_items` arrays of the old prompts are no longer asked for.
- **Cost.** Output tokens grow (items and quotes). Meet entries that were free now cost a
  transcript-sized input, usually through the map-reduce path. The budget caps
  (ADR-0013) bound it. A backfill of six months is a `sb resummarize --estimate` first.
- **Mixed catalog.** Entries summarized before this change have no items until they are
  re-summarized or their body changes. `sb items` and `sb recent` say how many entries in
  the range have no items, so an agent never presents an absent answer as "nothing".
- **Short Slack threads are not summarized** (`summary.min_chars`), so they carry no items.
  This is a known gap; the coverage figures show it.
- **Query-time work.** Candidate pairs and "mine" matching run in Rust at query time.
  They are deterministic and testable, and tuning them never needs a paid call.
- **Privacy.** An evidence quote is a few words of message text stored in the catalog, the
  same sensitivity as `details`. The account owner's names are sent to the summarizer with
  the body, which already contains them. Item text is data, not instructions, in the
  skill.
- **Slack IDs are not used.** The rendered conversation holds names, so matching is by
  name. Aliases therefore include every name form of the user.

## Alternatives considered

- **Keep only `sb recent` and let the agent extract.** Cheap, but the agent re-reads
  summaries on every question, cannot see state changes, and cannot answer a six-month
  question. Kept as step 1 of the rollout, not as the whole design.
- **Extract in a separate pass.** Re-reads every body a second time and doubles the
  input cost. A shared call costs only the extra output.
- **Inject open items into the summary prompt** so that the model emits `resolves`.
  Rejected for the three reasons in decision 5.
- **A second extraction level over threads or meeting series.** A Slack thread is
  already such an aggregate, and a Meet series had no key. The series key is stored now so
  that this can be added later without a refetch of new entries.
- **Decide "me" at extraction time.** A wrong alias would need a paid re-extraction.
- **Ask the user for their names.** Avoided on purpose; an override (`extra_aliases`) is
  allowed but not required.
- **Calibrated numeric confidence.** Models do not calibrate numbers. Three levels
  (`high`, `medium`, `low`) tied to what the input says are stable and explainable.
- **Auto-close stale items.** Silence is not completion.
