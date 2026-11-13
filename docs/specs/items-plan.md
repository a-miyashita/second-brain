# Implementation plan: items, recent entries and reconciliation

Related ADRs: 0002, 0005, 0013, 0017, 0024. Specs to read first: [items.md](items.md),
[summarization.md](summarization.md), [data-model.md](data-model.md),
[accounts-and-auth.md](accounts-and-auth.md#self-aliases).

Scope: items extracted with summaries, `sb recent`, `sb items` with query-time candidate
pairs, self aliases, Meet summarized by an LLM, and skill version 5.
Out of scope for the first release: the reconcile stage (phase B below), `same_as` links,
meeting-series aggregation entries, manual status overrides, and the MCP tools (`sb mcp`
is not built; the tools are specified in agent-integration.md and ship with it).

## Spikes (before the code that depends on them)

Spike results use no real data in the repository. Recorded responses and sample inputs are
scrubbed before they become fixtures (AGENTS.md).

| # | Question | Result goes to |
|---|---|---|
| S-I1 | **Extraction quality.** Run the draft v2 prompts with the Haiku-class profile on about 40 real entries (Slack threads and days, Meet transcripts, documents), reviewed with `sb review`. Measure: share of items whose quote is verified, wrong `done`/`dropped`, missed actions, duplicated facts between `action` and `accomplishment`, and how often a relative `due` is resolved correctly | The wording of the prompts, the verification rules (items.md "Validation"), `items.min_confidence` |
| S-I2 | **Cost.** Output tokens per entry with items, and input tokens of an hour-long Meet transcript (chunks and merge calls). Compare with `ESTIMATED_OUTPUT_TOKENS` and the budget defaults | `ESTIMATED_OUTPUT_TOKENS`, the cost note in source-google-meet.md, the suggested budget |
| S-I3 | **Slack alias fields.** For the own user in `users.list`: are `real_name`, `profile.display_name` and `profile.email` present for typical workspaces, including Enterprise Grid and guests? Does the rendering label (real name first) match what Gemini notes use for the same person? | The Slack row of the alias table, the matching rules |
| S-I4 | **Google alias fields.** Does the Calendar attendee with `self: true` carry `displayName` in a Workspace and in a personal account? How often does the Gemini "Invited" line contain the user's name with the same spelling as in the transcript's speaker labels? | The Google row of the alias table, whether the `profile` scope is ever needed |
| S-I5 | **Candidate pairs.** On a scrubbed sample with hand-labeled open/closed pairs: precision and recall of the rules in items.md for several similarity floors, for Japanese and English, with and without the shared-person rule | `items.candidate_min_similarity`, `items.candidate_days`, and the decision whether phase B is needed |
| S-I6 | **Transcript layout.** Check that the speaker labels of Meet transcripts survive the mechanical cleanup, so that first person statements can be tied to a speaker | The `meeting-summary/v2` rules and `account_owner` wording |

## Phase A — extraction, retrieval and aliases

Each task is a small commit (`feat:`, `refactor:`, `test:`, `docs:`). The order keeps the
tree green: kernel and store first, then the model output, then the commands.

1. **`sb-kernel`: types and pure rules.** `ItemKind`, `ItemStatus`, `Confidence`,
   `ItemDraft` (as parsed from the model), `Item`. `SummaryOutput` gains
   `items: Vec<ItemDraft>`; the `decisions` and `action_items` arrays are removed, because the
   sections are rendered from items. Pure functions, each with unit tests and no I/O:
   - `normalize_for_match` (NFKC, lowercase, whitespace, Markdown marks) and
     `verify_evidence(body, quote)`;
   - `validate_items(draft, body, entry) -> (Vec<Item>, ItemCounts)` implementing items.md
     "Validation";
   - `scope_key(source_kind, metadata)` and `normalize_meeting_title`;
   - `occurred_at` for Slack (from the rendered `HH:MM` line and date headers), Meet and
     documents;
   - `render_sections(items)` for `decisions` and `action_items`;
   - `AliasSet`, `matches_alias` (items.md matching rules, token-set equality);
   - `trigram_similarity`, `effective_status` and `is_stale` (all taking a `Clock`), and
     `candidates_for` over a slice of items.
2. **`sb-store`: migration `0005`** (`items`, `item_links`, indexes), `Catalog` methods
   to write and read items. `commit_summary` takes the validated items and replaces the
   entry's items in its transaction; committing a native summary deletes them. Queries for
   `sb items` and `sb recent`, including the `coverage` counts. Test with a temporary
   `SECOND_BRAIN_HOME`: replace on re-summarization, cascade on entry delete, native
   commit removes items, `(entry_id, ordinal)` uniqueness.
3. **`sb-llm`: prompts and parsing.** `conversation-summary/v2`, `meeting-summary/v2`,
   `document-summary/v4` with the item rules and the `account_owner` input; keep the old
   versions' text so that stored summaries stay attributed. `parse_output` reads `items`
   and treats a missing `items` as invalid (repair path). Map-reduce: chunk calls return
   items, `merge_prompt` carries them and asks for de-duplication and latest status.
   `SummaryInput` gains `account_owner: Option<Vec<String>>` (not part of the hash).
4. **`sb-pipeline`: wire the summarize stage.** Build `account_owner` from the aliases
   (names only, at most 8). After a summary returns, run `validate_items` against the full
   body, store the counts in `summaries.usage.items`, and pass the items to the commit.
   `resummarize` replaces items in the same transaction as before.
5. **Meet.**
   - Fetch: put `recurringEventId` into the queue hint and the entry metadata
     (`recurring_event_id`), and keep `self` and `displayName` per attendee. `normalize`
     reads them; `sb reextract` and `sb refetch` apply to old entries.
   - Policy: `summary.profile.google.meet` falls back to `summary.profile.default`, then to
     `native`. Remove the built-in default `native` of that key (`sb-store/src/settings.rs`)
     and keep an explicitly stored value. Update the settings screen text.
6. **Aliases** (accounts-and-auth.md). Slack: read the own entry of the user directory and
   its `profile.email` at `account add`, `auth login` and directory refresh (add an `email`
   read for the own member only). Google: derive names from stored Meet metadata at the end
   of the Meet fetch stage. Microsoft: read `name`, `preferred_username` and `email` from
   the ID token at `account add` and `auth login`. Write `aliases` into the account
   `config`; add `aliases` to `READ_ONLY_ACCOUNT_KEYS` in the settings screen; support
   `extra_aliases` in `sb config edit --account`.
7. **`sb-cli`: `sb recent` and `sb items`.** Arguments, the `today` date form, `--mine`
   with the `aliases.empty` error, JSON `sb.recent/v1` and `sb.items/v1` with `coverage`,
   the human tables, `--candidates`. Settings `items.*` as code defaults with
   `sb config get/set` validation. Tests for the JSON shape (a `schema` field, additive
   rules of cli.md).
8. **`sb-cli` `doctor`:** `items.coverage` and `accounts.aliases` (doctor.md).
9. **Skill version 5** in `crates/sb-setup/assets/skills/second-brain/`: `SKILL.md` section
   for questions without a keyword, trigger phrases, and the `references/items-guide.md`
   file described in agent-integration.md rule 8; `search-guide.md` points to it. Bump the
   embedded skill version so that `doctor skills.version` asks users to reinstall
   (`sb setup skills`).
    *Status:* tasks 1-9 are implemented (kernel `items.rs`, store migration `0005`, prompts
    v2/v4, pipeline `items.rs`, Meet series key and `self`, aliases, `sb recent`,
    `sb items`, doctor checks, skill version 5). The spikes S-I1 to S-I6 still need real
    data and real API responses and are **not done**; the rules marked "tune" in items.md
    (similarity floor, stale window, confidence floor) are code defaults until then.
10. **Docs and release notes.** Mark the spec text that is now implemented, add the
    `CHANGELOG.md` entry through `node scripts/release.mjs` at release time (never by
    hand, AGENTS.md), and describe the backfill commands in the manual.

### Rollout for the user's catalog

1. Release with Phase A. New and changed entries get items from the next sync.
2. `sb summarize --estimate`: the Meet switch makes native Meet entries `pending`; read the
   cost against the budget caps before the first sync.
3. Backfill the past in steps, cheapest value first (Slack threads the user took part in,
   then Meet), with `sb resummarize --since <N>d --profile <P> --estimate`, then without
   `--estimate`. Interrupt freely: a re-run skips entries already at the target prompt.
4. Use `sb items --kind action --status open --mine` for a week. If agents or the user
   spend effort matching closing messages to open items by hand, build phase B.

## Phase B — reconcile stage (after use shows it is needed)

Decision rule: S-I5 precision and the experience of step 4 above. Not part of the first
release.

1. **Migration `0006`:** `reconcile_queue(entry_id PK, reason, enqueued_at)` and
   `llm_usage.purpose TEXT NOT NULL DEFAULT 'summary'`.
2. `item_reconcile` prompt `item-reconcile/v1`, parsing and validation (items.md
   "Reconcile stage").
3. Queueing in `commit_summary` (the entry, and the link partners found before the cascade
   deletes the links).
4. The stage after summarize in `sb sync`, and `sb reconcile` (`--rebuild`, `--estimate`,
   `--dry-run`, limits, graceful stop). Same budget gate; the ledger row is written with
   the links.
5. `sb items` fills `resolved_by` and uses links in the effective status; `--candidates`
   skips pairs that already have a link. `sb budget --by-purpose` (optional).
6. Doctor: `reconcile.backlog` (queue size).

## Tests

- **Pure rules** in `sb-kernel`: table-driven tests for evidence verification (Japanese,
  Markdown marks, whitespace), downgrade rules, `occurred_at` for Slack with and without
  date headers, scope keys, alias matching (token order, single-token names, emails),
  trigram similarity, effective status and staleness with an injected `Clock`, candidate
  pairs (window, scope, shared person, same-entry exclusion).
- **Store:** migrations apply on an existing `0004` catalog; replace-on-resummarize;
  cascade; the `coverage` counts on a mixed catalog (v1 summaries, native, skipped,
  v2 with items).
- **LLM:** stub executables and canned answers for valid output, output without `items`
  (repair), output with a fabricated quote (downgraded or dropped), map-reduce with
  duplicate items across chunks, and an unchanged `input_hash` when only the alias hint
  changes.
- **Meet:** a `wiremock` Calendar response with `recurringEventId` and `self` attendees;
  the metadata and the scope key; the profile fallback chain.
- **CLI:** `sb recent` and `sb items` JSON snapshots on a fixture catalog (no real names,
  channels or IDs; fixtures follow AGENTS.md), `--mine` without aliases, `--due-by`,
  `--stale`, `--candidates`.
- **No real network access** in any test.

## Risks

| Risk | Mitigation |
|---|---|
| The model invents completions | Quote required and checked; unverified `done` becomes `open`; nothing is closed by silence or by similarity |
| Names do not match across sources ("Dana" vs "山田 太郎") | Aliases from every account, token-set matching, `basis` in the output, `extra_aliases`; the failure is a missing item, not a wrong one |
| Short Slack threads carry no items | `coverage.unsummarized` and `sb recent` with `details_excerpt`; lower `summary.min_chars` is a cost decision for the user |
| Cost of Meet transcripts | Budget caps, `--estimate` before the first sync, rollout in steps |
| Mixed catalog gives partial answers | `coverage` in every answer and the skill rule not to say "nothing" |
| Item references change on re-summarization | `item_ref` is documented as valid until the entry is re-summarized; no manual state is stored on items |
