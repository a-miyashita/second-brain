# Implementation plan: folder sync and link ingest

Related ADRs: 0008, 0012, 0014, 0016, 0021, 0022, 0023. Specs to read first:
[folder-sync.md](folder-sync.md), [link-ingest.md](link-ingest.md),
[summarization.md](summarization.md) ("Document hints"), [source-documents.md](source-documents.md),
[ingest.md](ingest.md).

Scope: folder sync for Google Drive and SharePoint/OneDrive, ingest of Google Drive and
SharePoint/OneDrive links found in Slack entries, `document-summary/v3` with `location` and
`referenced_in`, and `summary.min_chars.<source_kind>`.

Out of scope: change feeds (Drive Changes, Graph `delta`), enumeration of sites or
drive roots, links in sources other than Slack, links to web pages, refreshing linked
documents by sync, deleting entries.

## Status

Tasks 1 to 14 are done, in the order of the plan: the kernel types and the shared folder
walk, migration `0004`, `document-summary/v3` and the per-kind threshold, the Slack link
extraction, the link stage, folder sync for both providers, `sb folder`, `sb scan-links`,
the doctor checks, the setup and settings screen, the skill (version 4) and the manuals.
Differences from the text below are written into the specs (for example, the per-folder
breakdown is returned by `sb folder check`, not by `sb sync --json`).

**Still open: the spikes.** They need real accounts and tenants, so the code was written
and tested against mock servers. Before a release, check at least S-F1 (Graph `children`
with `Files.Read.All` only, and the `delta` limit) and S-F2 (Drive `modifiedTime`
semantics, shared drives). If the facts differ from ADR-0022 decision 5, add an
Amendment to the ADR.

## Spikes (before the code that depends on them)

| # | Question | Result goes to |
|---|---|---|
| S-F1 | Does `GET /drives/{drive}/items/{item}/children` work with `Files.Read.All` only, for (a) a folder resolved from a SharePoint URL by `shares`, (b) a OneDrive folder, (c) a folder shared into the user's OneDrive (`remoteItem`), (d) a personal OneDrive folder? Is `delta` indeed limited to the drive root on SharePoint and OneDrive for Business? Page sizes and throttling for a folder of 2000 files | folder-sync.md "Listing", ADR-0022 decision 5 (an Amendment if the facts differ) |
| S-F2 | Drive `files.list` with `includeItemsFromAllDrives` for a folder in a shared drive and for a folder shared with the user: do all children appear, and with which `modifiedTime` semantics (content change vs metadata change)? Does a shortcut to a folder or file report `shortcutDetails.targetId`? | folder-sync.md "Listing" and the scope rule on `modifiedTime` |
| S-F3 | How do links appear in real Slack messages: mrkdwn `<url\|label>`, bare URLs, rich text `link` elements, unfurl attachments (`from_url`, `original_url`), links inside quoted or forwarded messages? Which Google and SharePoint URL forms are common (with `/u/0/`, `/a/domain/`, `?e=` parameters)? | The extraction rules and fixtures of link-ingest.md |
| S-F4 | How often does a Meet notes folder overlap with a folder a user would sync (Drive IDs present as `google.meet`)? | The wording of `docs.meet_overlap` and the guide |
| S-F5 | Cost of a listing run: requests and time for a folder with 2000 files in 100 subfolders on each provider, with and without `recursive` | The defaults `max_depth`, `max_files`; whether `--deep` should matter |

Spike results use no real data in the repository. Recorded responses are scrubbed before
they become fixtures.

## Tasks

Each task is a small commit (`feat:`, `refactor:`, `test:`, `docs:`). Tests are part of the
task that adds the code.

1. **`sb-kernel`: types.** `SummaryInput.location` and `.referenced_in` (optional, serde
   defaults, so stored data still loads); `DiscoveredLink` and `Normalized.links`;
   `EntryOrigin::Link`; the pure link classification (`classify_link(url) -> Option<LinkKind>`
   with the link key); `FolderEntry`, the `FolderLister` trait and the generic planning
   function of folder sync (scope by `covered_since`, compare with the stored state,
   build `QueueItem`s and the coverage update), so that both provider crates depend only on
   the kernel (architecture.md). Update exhaustive matches.
2. **`sb-store`: migration `0004`.** `discovered_links` and its index; catalog methods
   (`upsert_discovered_link` with first-wins, `pending_links`, `record_link_result`,
   `reset_links`, counts for doctor and `scan-links`). Settings validation for
   `links.*`, `summary.min_chars.<kind>` and the account keys `folders`, `ingest_links`,
   `ingest_link_hosts` (values, kinds, globs, ranges).
3. **`sb-llm`: `document-summary/v3`.** Prompt with the labeled inputs and rules of
   ADR-0023; v2 stays for attribution. Tests: the prompt text with and without each input,
   delimiters, an injection attempt in the body, in `location` and in `referenced_in`, and
   that v2 inputs still render.
4. **`sb-pipeline`: thresholds per kind.** `summary.min_chars.<kind>` with the built-in
   default `0` for `google.doc` and `microsoft.file`, in both threshold checks
   (summarization.md). Update the `skipped` message.
5. **`sb-slack`: links and excerpt.** Extraction and excerpt in `normalize`
   (link-ingest.md), from the raw messages and the user snapshot. Fixtures from S-F3.
6. **`sb-pipeline`: record links.** On commit and in `reextract`, upsert the links of
   the entry into `discovered_links` in the entry's transaction.
7. **`sb-google`: folder sync.** A `FolderLister` over `files.list` (shared drives,
   shortcuts, paging, cycle guard), the folder URL resolver for `sb folder add`,
   `GoogleSource::sync` running Meet sync and folder sync, and the fetch using the queue
   `hint` for `folder_path`. `normalize` maps `folder_path` to `location`.
8. **`sb-microsoft`: folder sync.** A `FolderLister` over `children` (`remoteItem`,
   `package`, paging, throttling), the folder resolver over `shares`, `supports_sync()`
   when folders exist, the same hint handling.
9. **`sb-pipeline`: folder sync wiring.** Run folder sync for the selected accounts
   (`--source`, `--since`/`--until`, `--dry-run`), coverage in `sync_state`, issues
   `folder.unavailable` and `folder.too_large`, stats for `sb.sync/v1`.
10. **`sb-pipeline`: link stage.** Selection, account probing (shared with ingest), fetch
    with `link_context` and `discovered_via`, `origin = link`, result recording, backoff,
    `--source links`, limits and interruption, stats.
11. **`sb-cli`.** `sb folder add|list|remove|check`; `sb scan-links` (read-only and
    `--ingest`); `sb sync --source` values; `sb.sync/v1` fields; `sb.folder/v1` and
    `sb.scan-links/v1`; `sb config` keys; help texts; usage errors (file URL for
    `folder add`, `--until` without `--since`).
12. **`sb-cli` `doctor`.** `folders.access`, `folders.too_large`, `links.pending`,
    `docs.meet_overlap`.
13. **`sb-setup` and the TUI.** The accounts section lists, adds and removes folders; the
    settings section has the link settings; the Slack account screen has `ingest_links`
    and the host list. Update the skill assets (`SKILL.md`, `references/ingest-guide.md`):
    the agent rules stay, with the note on user-configured sync and links
    (agent-integration.md). Bump the skill version, because `doctor` compares it with the
    binary. The MCP `scan_links` gets `kind?`.
14. **Docs outside `docs/`.** `README.md` (a "Folders and links" guide with the
    `sb folder add` steps, the settings and the cost advice: `sb folder check`,
    `sb sync --estimate`), `AGENTS.md` / `CLAUDE.md` only if the layout changes, `manuals/`,
    `CHANGELOG.md` (through `scripts/release.mjs` at release time only).

Order and dependencies: spikes S-F1 to S-F5 (S-F1 and S-F2 before 7 and 8, S-F3 before 5,
S-F4 and S-F5 before 12 and the defaults) → 1 → 2 → {3, 4, 5} → 6 → {7, 8} → 9 → 10 → 11
→ 12 → 13 → 14. Tasks 3 and 4 are independent of the rest and can ship first: they only
change summaries.

## Tests

The tests named in [folder-sync.md](folder-sync.md#tests) and
[link-ingest.md](link-ingest.md#tests), plus:

- **Resume** (ADR-0012): interrupt a sync after the folder enqueue, after a chunk, and in
  the link stage between a fetch and its commit; the final catalog equals that of an
  uninterrupted run.
- **One entry, three paths:** the same Drive file reached by `sb ingest`, by a folder and by
  a link is one entry, and keeps its first origin.
- **Always summarized:** a 100-character Google Doc is summarized by default; a cap leaves
  it `pending`; a per-kind override restores `skipped`.
- **No new network targets:** a Slack message with a link to an arbitrary host makes no
  request to it (the mock server asserts no other host is contacted).
- **Privacy:** logs at `info` contain no URL, excerpt, folder name or file name.

## Definition of done

`cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings` and
`cargo test --workspace` pass on the three platforms; the specs above match the behavior
(spec changes are made in the same commits as the code that diverges); the skill version
is bumped; `sb doctor` is clean on a catalog that uses none of the features.
