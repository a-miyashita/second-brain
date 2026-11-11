# Ingest of document links found in Slack

Related ADRs: 0002, 0008, 0012, 0013, 0014, 0021, 0022, 0023.

Supported Google Drive and SharePoint/OneDrive file links in Slack entries can be ingested by
`sb sync`, for the kinds of link the user turned on. The resulting entries are ordinary
`google.doc` and `microsoft.file` entries with `origin = link`. Scope: Slack entries
(`slack.thread`, `slack.day`) only.

## Settings

| Setting | Default | Meaning |
|---|---|---|
| `links.ingest_kinds` | `[]` | Source kinds whose links are ingested: any of `google.doc`, `microsoft.file`. Empty = none |
| Slack account `config.ingest_links` | (absent) | Overrides `links.ingest_kinds` for that workspace. `[]` = none. Absent = the global value |
| Slack account `config.ingest_link_hosts` | `[]` | Globs on the host name (`contoso.sharepoint.com`, `*.sharepoint.com`). Empty = no restriction. Applies to the links of that workspace |
| `links.max_per_run` | `50` | Links fetched by one link stage (shared by all workspaces) |
| `links.context_chars` | `1200` | Size limit of the Slack excerpt (ADR-0023) |
| `links.max_attempts` | `6` | Failures after which a link is abandoned |

Values are validated by `sb config set` and `sb config edit --account <id>`. An unknown
kind is rejected; `web.page` and `local.file` are not valid values (ADR-0022 decision 9).

Discovery (below) does not depend on these settings. They decide only what the link stage
processes, so enabling a kind later also covers links found earlier.

## Finding links

`Source::normalize` of a Slack entry returns `links: Vec<DiscoveredLink>`:

```text
DiscoveredLink { url, message_ts, excerpt }
```

It is a pure function of the stored raw messages and the directory snapshot.

- **Where:** in each message, the mrkdwn forms `<url>` and `<url|label>`, `link` elements
  of Block Kit blocks (rich text, section, context), and the attachment fields
  `title_link`, `from_url` and `original_url`. Bare URLs in `text` that Slack did not wrap
  are read too. Files and images are not.
- **Classification** by URL alone, in this order. Anything else is ignored:

  | Kind | URL |
  |---|---|
  | `google.doc` | `docs.google.com/document/d/<id>`, `/spreadsheets/d/<id>`, `/presentation/d/<id>`, `drive.google.com/file/d/<id>`, `open?id=<id>` or `uc?id=<id>`; with an optional `/u/<n>` or `/a/<domain>` prefix. Published-to-web `/d/e/...`, forms, folders (`/drive/folders/...`) and `drive.google.com/drive/...` are not links to ingest |
  | `microsoft.file` | host `*.sharepoint.com`, `onedrive.live.com` or `1drv.ms` (ingest.md). The URL form says nothing about whether it is a file, a folder or a page; Graph decides at fetch time (`NotApplicable`) |

  Slack permalinks (`*.slack.com/archives/...`) are ignored.
- **Link key** (`link_key`): `google:<fileId>` for Google, so that every URL form of one
  file collapses; `ms:<sha256 of the URL with its fragment removed>` for Microsoft.
- **Excerpt** (ADR-0023 decision 4), at most `links.context_chars` characters:
  1. a header `<conversation label>, <local date>` (the label of the entry title, in
     English; the date of the message);
  2. the message that holds the link, with the link replaced by `[linked document]`
     (a sharing URL can be a credential, and the model does not need it);
  3. the first message of the thread, if it is another message;
  4. the messages immediately before the link message, then the one after, nearest first,
     while the limit allows.

  Each line is `Name: text`, with the speaker names resolved as in the `details` section.
  A message is cut at 600 characters. The excerpt is deterministic for the same input.

The pipeline records the links of every entry it commits or updates (including
`sb reextract`) with `INSERT ... ON CONFLICT DO NOTHING` into `discovered_links`, in the
same transaction as the entry (ADR-0012). The first reference wins: its entry and excerpt
are kept. A link whose Slack entry is deleted keeps its row (the entry reference becomes
`NULL`), so a pending link can still be fetched; it has no excerpt then.

## Table `discovered_links` (migration `0004`)

| Column | Type | Notes |
|---|---|---|
| `account_id` | TEXT FK → accounts ON DELETE CASCADE | The Slack account whose entry referenced the link |
| `link_key` | TEXT | See above. Primary key with `account_id` |
| `source_kind` | TEXT | `google.doc` or `microsoft.file` |
| `url` | TEXT | The URL as found (without the fragment). May carry a sharing token: never logged, printed only by `scan-links` |
| `host` | TEXT | Lower-case host, for `ingest_link_hosts` |
| `first_entry_id` | INTEGER NULL FK → entries ON DELETE SET NULL | The first referencing Slack entry |
| `first_seen_at` | TEXT | |
| `excerpt` | TEXT NULL | The excerpt of the first reference |
| `status` | TEXT | `pending`, `ingested`, `unavailable`, `abandoned`, `not_applicable`, `duplicate` |
| `entry_id` | INTEGER NULL FK → entries ON DELETE SET NULL | The entry made from the link, or the existing entry for `duplicate` |
| `attempts` | INTEGER | Failures counted so far |
| `next_attempt_at` | TEXT NULL | Not before this time (UTC) |
| `last_error` | TEXT NULL | The reason, as shown to the user |
| `updated_at` | TEXT | |

Index: `(status, source_kind)`. The table is mutable state, not a ledger: `sb account
remove --purge` removes the rows of that Slack account through the foreign key.

## The link stage

Runs in `sb sync` after the account syncs and before summarization (ADR-0022 decision 16),
when at least one Slack account was synced in the run, or with `--source links` alone.

1. **Select.** For each Slack account in the run, take the `discovered_links` rows with
   `status = pending`, `source_kind` in the account's effective kinds, a host that passes
   `ingest_link_hosts`, and `next_attempt_at` empty or in the past. Order by
   `first_seen_at`, newest first. Take at most `links.max_per_run` rows over all accounts.
2. **Process** with up to 4 links in flight, on the ingest path (ingest.md "Flow"):
   classify, choose the account, `locate`, duplicate checks, `fetch`, commit. Differences:
   - the choice of account probes the eligible accounts of the kind in order of creation
     (ingest.md "Choosing the ... account"); there is no `--account`;
   - the fetch metadata gets `link_context` (the excerpt) and `discovered_via`
     (`{"entry_uid", "title", "url"}` of the Slack entry, when it still exists);
   - the entry gets `origin = link` (an existing entry keeps its origin);
   - a document is summarized by the usual stage afterwards, with `location` empty and
     `referenced_in` from `link_context` (ADR-0023).
3. **Record the result** in the same transaction as the commit:

   | Result of the ingest path | `status` | Other fields |
   |---|---|---|
   | `created`, `updated`, `unchanged` | `ingested` | `entry_id` |
   | `duplicate` (Drive file ID is a `google.meet` entry; or the entry exists) | `duplicate` | `entry_id` = that entry |
   | `not_applicable` (folder, unsupported type, no text, OneNote, ...) | `not_applicable` | `last_error` = the reason |
   | `failed`: not found or no access on every eligible account | `unavailable` | `attempts + 1`, `next_attempt_at` by the schedule below, `last_error` |
   | `failed`: transient (network, `429`, `5xx`) or the budget/limit stopped the run | stays `pending` | `attempts` unchanged; tried again in the next run |
   | No eligible account of the kind | stays `pending` | `last_error` = "no Microsoft account" (or Google); not counted |
   | Every eligible account is `needs_reauth` | stays `pending` | not counted |

4. **Backoff** for `unavailable`: the next attempt is after 1, 3, 7, 14, then 30 days. When
   `attempts` reaches `links.max_attempts`, the status becomes `abandoned` and the link
   is not tried again unless `sb scan-links --ingest --retry` resets it (`attempts = 0`).
   The reason is that a link to a document the user cannot read costs a request, and
   in the case of several accounts, one request per account, in every run.
5. **Limits.** The run's cancellation, `--time-limit` and budget stop the stage between
   links. Fetching is cheap, so the budget matters only for the summaries that follow
   (ADR-0013); entries stay `pending` for them.
6. **Concurrency with other stages.** The links of one run are committed in chunks of
   `pipeline.commit_batch`. A link stage interrupted between a fetch and its commit
   repeats that fetch (ADR-0012).

A linked document is **not refreshed** by later runs: once `ingested`, its link is not
visited again. `sb refetch --source google.doc` (or `microsoft.file`) refreshes entries on
demand.

## `sb scan-links`

```text
sb scan-links [--since AGE] [--account ID] [--kind K]... [--limit N]
              [--ingest [--retry]]
```

| Option | Meaning |
|---|---|
| `--since AGE` | Slack entries whose `source_updated_at` is within this age. Default: all |
| `--account ID` | Only this Slack account |
| `--kind K` | Only these kinds. With `--ingest` it also **overrides the enabled kinds** for this run (the user's explicit choice) |
| `--limit N` | At most N links listed or ingested. Default `links.max_per_run` |
| `--ingest` | Record the links of the selected entries (`reextract` of the links), then run the link stage on them. Takes the sync lock. Without it the command is read-only |
| `--retry` | With `--ingest`: also try `unavailable` links now (ignoring `next_attempt_at`) and reset `abandoned` ones |

Without `--ingest` it reads stored raw data and `discovered_links`, writes nothing and
takes no lock. Output, one line per link, newest first:

```text
pending      google.doc      docs.google.com    #design-review 2026-11-02 "Design review ..."
ingested     microsoft.file  contoso.sharepoint.com   DM: Alice ... -> entry 01J9...
unavailable  google.doc      docs.google.com    not found or no access (next try 2026-11-14)
```

`--json`: `{"schema": "sb.scan-links/v1", "counts": {...}, "links": [{"url", "host",
"source_kind", "status", "from_entry_uid", "from_title", "message_date", "entry_uid",
"last_error", "next_attempt_at"}]}`. The URL is printed because the user needs it; it is
never written to logs (agent-integration.md for the MCP tool `scan_links`).

Exit codes: 0 for success, whatever the link statuses. 1 if an ingested link failed for a
reason other than "not found or no access". 64 usage error, 75 sync lock held.

## Stats and JSON of `sb sync`

`sb.sync/v1` adds `links` (additive): `{"discovered": n, "pending": n, "ingested": n,
"unavailable": n, "not_applicable": n, "duplicate": n, "abandoned": n,
"deferred_by_limit": n}` for the run. The human output prints one line.

## Doctor

| ID | Check | Severity |
|---|---|---|
| `links.pending` | Links in `pending` whose kind is enabled but have no eligible account (named per kind), and the number of `abandoned` links | info |
| `docs.meet_overlap` | A Drive file ID that is both a `google.doc` and a `google.meet` entry (folder sync of a Meet notes folder) | info |

See also [folder-sync.md](folder-sync.md) for the folder checks.

## Security

- **The URL is never requested by `sb`** (ADR-0022 decision 9). Google receives the file ID.
  Graph receives `shares/u!{base64url(url)}` on `graph.microsoft.com`. Both are the calls of
  `sb ingest`. The web address guard is not involved, and no other host is contacted.
- A link can make `sb` read only what one of the user's own accounts can read, and only for
  the kinds and hosts the user listed. `links.max_per_run`, the backoff and the budget caps
  bound the effect of a thread full of links.
- The excerpt and the document text are untrusted data for the summarizer (ADR-0023).
- Logging: the URL and the excerpt are never logged above `trace`; `info` shows counts and
  the host.

## Tests

- Extraction fixtures (scrubbed): mrkdwn `<url|label>`, a bare URL, Block Kit `link`
  elements, unfurled attachments, a Slack permalink (ignored), a Drive folder (ignored), a
  published Google Doc (ignored), several URL forms of one Google file (one key).
- Excerpt: the link in the first message, in a reply, at the edge of the limit, a long
  message, the link replaced by `[linked document]`, determinism.
- Stage: kinds off by default; the account override; the host list; no eligible account
  (stays `pending`, not counted); two accounts with one that cannot read; `unavailable`
  backoff and `abandoned`; a transient failure; `--time-limit` between links; an existing
  entry (`unchanged` → `ingested`); a `google.meet` duplicate; `origin` kept for an
  existing entry; interruption and resume.
- `scan-links` read-only (no writes, no lock), `--ingest`, `--kind` override, `--retry`.
- `wiremock` for Drive and Graph; no real URLs or IDs in fixtures.
