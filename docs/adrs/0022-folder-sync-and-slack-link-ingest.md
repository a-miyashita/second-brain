# ADR-0022: Folder sync for Google Drive and SharePoint/OneDrive, and ingest of document links found in Slack

- Status: Accepted
- Date: 2026-11-11

## Context

`sb ingest` reads one Google Drive or SharePoint/OneDrive file at a time (ADR-0014,
ADR-0021), and only when the user or an agent names it. Two needs remain:

1. Some folders hold documents that should simply be part of the knowledge base (minutes,
   design documents, specifications). The user wants `sb sync` to bring in the supported
   documents of such a folder, and to keep them up to date.
2. People paste links to documents into Slack threads. The thread explains why the
   document matters, and the document holds the detail the thread lacks. The user wants
   the supported links of a thread ingested too, for the kinds of link the user chooses.

ADR-0014 decision 8 ("No crawling. Directories, Drive folders, sitemaps and link
following are out of scope") and ADR-0021 decision 2 ("On-demand only (no `sync`)")
rule both out. ADR-0008 says link discovery "never ingests automatically". This ADR
replaces those three statements for the cases below. Everything else in those ADRs stays.

Facts that shape the design:

- Both providers key a file by an ID that does not change when the file is renamed or
  moved: the Drive file ID and `<driveId>/<itemId>`. Folder sync, link ingest and
  `sb ingest` therefore reach the **same entry** for the same file (ADR-0002 natural key).
- The existing `sync_queue` is keyed by account, source kind and source ID. A Slack
  account cannot queue a `microsoft.file`: that key is known only after a Graph call
  (ADR-0021 amendment on `locate`), and the account that can read the file is not known.
- Slack content is written by other people. Ingest triggered by a link must not give
  them a way to make `sb` fetch arbitrary URLs.

## Decision

### Common

1. **No new source kinds and no new permissions.** Folder sync and link ingest produce
   `google.doc` and `microsoft.file` entries. Google keeps `drive.readonly`. Microsoft
   keeps `Files.Read.All`; `Sites.Read.All` and site or library enumeration are still not
   used (ADR-0021 alternatives). A folder is always named by the user, by its URL.
2. **Opt-in everywhere.** A folder is synced only after `sb folder add`. A kind of link is
   ingested only when the user lists it in a setting whose default is empty.
3. **Entries are not deleted by these features.** A file removed from a folder, or a link
   whose document is gone, leaves the stored entry as it is (ADR-0014 accepts link rot).

### Folder sync

4. **Configured per account, added by URL.** `sb folder add <account> <url>` resolves the
   folder (a Drive folder ID, or a `shares` lookup for Graph), checks that the account can
   list it, and stores the folder in the account's `config.folders` with its provider IDs.
   Google and Microsoft accounts both can have folders. Details: [folder-sync.md](../specs/folder-sync.md).
5. **A run lists the tree and compares; it does not use change feeds.** Each `sb sync`
   lists the folder tree (breadth first, depth-limited, no cycles), compares every
   supported file with its stored `fetch_state` (modified time or ETag), and puts new and
   changed files into `sync_queue` in one transaction. The queue is then drained by the
   existing machinery (ADR-0012). Reasons: the Drive Changes API and Graph `delta` have
   different scoping rules (as far as is known, Graph `delta` on SharePoint and OneDrive
   for Business works only at the drive root, not on a subfolder; spike S-F1 confirms),
   and listing is the same idea for both providers and needs no state that can drift.
   The cost is one list request per page of children, for every folder, in every run.
6. **The window follows ADR-0016, by modification time.** Each folder has `covered_since`.
   The first run covers files modified since `run_started_at - initial_days` (the folder's
   own `initial_days`, else `sync.initial_days`). `sb sync --since X` extends the window
   backwards. Files older than `covered_since` are not entered. Invariant: every
   supported file in the folder with a modification time at or after `covered_since` is an
   entry or is in `sync_queue`.
7. **Only supported types are entered.** The listing skips what `google.doc` and
   `microsoft.file` would answer `NotApplicable` for (images, archives, forms, OneNote,
   legacy `.doc`, ...) and counts them. Optional `include` and `exclude` globs narrow the
   set further.

### Links in Slack

8. **Links are found in `normalize`, and kept in a table.** `normalize` of a Slack entry
   returns the links it contains (pure, deterministic). The pipeline records each
   supported link once in a new table `discovered_links` (migration `0004`), whatever the
   settings say. A table, not `sync_queue`, because the target account and key are not
   known at discovery time (see Context), and because a link discovered while its kind
   is disabled must be usable when the kind is enabled later.
9. **Which links are supported is a closed list.** Google Docs, Sheets, Slides and Drive
   file links (`google.doc`), and SharePoint, OneDrive and `1drv.ms` file links
   (`microsoft.file`), using the classification already in ingest.md. Never `web.page`.
   Folder links are ignored (a folder is a sync target, not a link). Slack permalinks are
   ignored. The URL itself is never fetched by `sb`: Google is asked for the file ID, and
   Graph for `shares/{encoded URL}`, as for `sb ingest`. A pasted link therefore cannot
   make `sb` contact a host of the poster's choice.
10. **The user chooses the kinds.** `links.ingest_kinds` (global, default `[]`) and the
    Slack account key `ingest_links` (overrides the global value; `[]` turns it off for
    that workspace) list the source kinds to ingest. Optional `ingest_link_hosts` (globs on
    the host name) narrows further, for example to the organization's own SharePoint host.
    A kind with no eligible account is not an error: the links stay `pending` and
    `sb doctor` says so.
11. **A link stage runs after the source syncs.** It takes pending links of enabled kinds,
    probes the eligible accounts with the ingest rules, fetches and commits entries with
    `origin = link`, at most `links.max_per_run` (default 50) per run. A link that cannot
    be read is retried with growing delays and then abandoned, so a dead link does not
    cost a request in every run. Details: [link-ingest.md](../specs/link-ingest.md).
12. **Backfill is explicit.** `sb scan-links` (specified in ADR-0008, phase 2) lists the
    candidate links of existing Slack entries and changes nothing. `sb scan-links --ingest`
    records them and runs the link stage. Without `--ingest` it stays read-only.
13. **Each link keeps its first referencing entry.** The entry records
    `metadata.discovered_via` (entry UID, title, URL of the Slack entry) and the excerpt
    that becomes summary context (ADR-0023). Later references to the same document add
    nothing. A linked document is not refreshed by `sb sync`; `sb refetch` does that.

### Pipeline and schema

14. **`Normalized` gets an optional list of links.** Additive change to `sb-kernel`. The
    `Source` trait does not change; a Google adapter dispatches folder sync next to Meet
    sync (`GoogleSource::sync`), and the Microsoft adapter reports `supports_sync()` when
    the account has folders.
15. **`entries.origin` gets the value `link`.** `sync` is used for folder-synced entries.
    An existing entry keeps its origin when another path reaches it.
16. **Stage order of `sb sync`:** account syncs (including folder listing and queue
    drains) → link stage → summarize → index. The sync lock, interruption, budget caps and
    `--time-limit` apply to every stage.

## Consequences

- Documents of a configured folder, and linked documents, become searchable and
  summarized (ADR-0023) without the user naming each one. The first sync of a large folder
  can be expensive: `sb folder add` and `sb folder check` print how many files are in
  scope, `sb sync --estimate` prices the pending summaries, and the weekly and monthly
  caps (ADR-0013) still stop spending.
- Every run lists every configured folder. At personal scale (hundreds to a few thousand
  files) this is a few requests per folder. If a folder grows so large that this hurts, a
  change feed can be added behind the same interface without changing the data model.
- A Drive folder that holds Gemini meeting notes can yield a `google.doc` entry and a
  `google.meet` entry for the same file ID. ingest.md already reports a `google.meet`
  match as `duplicate`, but the opposite order is not detected by Meet sync. `sb doctor`
  reports such pairs (info), and the guide says not to sync the Meet notes folder.
- A moved but unchanged file keeps its stored folder path until it changes or is
  refetched. The path is a hint (ADR-0023), not a key.
- Entries made by a feature that is later turned off stay in the catalog.
- `scan-links` is no longer "read-only" as ADR-0008 says, only by default.
- A document linked in Slack that the user cannot read is recorded as unavailable and not
  retried forever. Nothing is shown to Slack participants: the feature is read-only on
  every service.
- Prompt injection: a poster can influence **which** document a link points to, within
  what the user can read, and the user's own LLM provider then sees that document. Opt-in
  kinds, the host list, `links.max_per_run` and the budget caps bound this. The document
  text and the Slack excerpt reach the summarizer as untrusted data (ADR-0023).

## Alternatives considered

- **Drive Changes API and Graph `delta` for incremental sync.** Cheaper per run, but
  different per provider, with tokens to store and invalidate, and (for SharePoint and
  OneDrive for Business) not available on subfolders as far as is known. Rejected for now.
- **New source kinds (`google.folder_doc`).** Would break the natural key and duplicate
  entries reached by `sb ingest` or a link. Rejected.
- **Queue links in `sync_queue`.** The Microsoft key is unknown before a Graph call and
  the account is unknown, so the row cannot be written. Rejected (decision 8).
- **Ingest every URL in Slack, including web pages.** Makes the sync a crawler steered by
  other people and sends bearer-less requests to arbitrary hosts. Rejected (decision 9).
- **Per-link prompts or confirmation.** Defeats unattended `sb sync`. The opt-in kind list
  is the control.
- **Delete entries when a file leaves its folder.** Raw data is the source of truth
  (ADR-0002) and link rot is accepted (ADR-0014). Rejected.
- **A folder-wide `context` text.** Considered and dropped: the folder name already says
  what the user knows about the folder, and a fixed text can be wrong for files in it
  (ADR-0023).
- **Enumerate a whole SharePoint site or OneDrive.** Needs `Sites.Read.All` and
  administrator consent. Rejected (decision 1).
