# Folder sync (Google Drive, SharePoint, OneDrive)

Related ADRs: 0002, 0012, 0014, 0016, 0021, 0022, 0023.

`sb sync` brings in the supported documents of folders that the user added to an account.
Entries are ordinary `google.doc` or `microsoft.file` entries ([source-documents.md](source-documents.md)):
the same natural key, fetch, extraction and normalization as `sb ingest`. This file
specifies what is new: the folder settings, the listing, the scope and the commands.

## Folder settings

Stored in the account `config` under `folders`, a list. Google accounts (any with
`drive.readonly`) and Microsoft accounts can have folders.

```json
{
  "folders": [{
    "key": "1AbCdEf...",
    "url": "https://drive.google.com/drive/folders/1AbCdEf...",
    "label": "Minutes",
    "recursive": true,
    "max_depth": 8,
    "include": [],
    "exclude": [],
    "initial_days": null,
    "max_files": 5000
  }]
}
```

| Key | Default | Meaning |
|---|---|---|
| `key` | set by `sb folder add` | Google: the folder ID. Microsoft: `<driveId>/<itemId>`. Identifies the folder in `sync_state` and in results. Not editable |
| `url` | as given | For display only |
| `label` | the folder's name | Name shown in output. Not used as context (ADR-0023) |
| `recursive` | `true` | Also list subfolders |
| `max_depth` | `8` | Folder levels below the root that are listed. `0` = the root only. Deeper folders are skipped and counted |
| `include` | `[]` | Globs on the file name or the path relative to the root (`/` separators, case-insensitive). Empty = every supported file |
| `exclude` | `[]` | Same syntax. Checked after `include` and wins. Built-in excludes: `~$*`, `.~lock.*`, `Thumbs.db`, `.DS_Store`, `desktop.ini` |
| `initial_days` | global `sync.initial_days` (`30`) | Look-back of the first run for this folder (ADR-0016) |
| `max_files` | `5000` | Files in scope above this number stop the folder's sync (below) |

There is no free-text context for a folder (ADR-0023). `sb config edit --account <id>`
edits the list; `sb folder add` is the supported way to add one.

## Commands

```text
sb folder add <account> <url> [--label L] [--no-recursive] [--max-depth N]
              [--include GLOB]... [--exclude GLOB]... [--initial-days N] [--max-files N]
sb folder list [--account ID]
sb folder remove <account> <label-or-key>
sb folder check [<account> [<label-or-key>]] [--since AGE]
```

- **`add`** resolves the URL (below), lists the folder once (names and counts only), stores
  the entry in `config.folders`, and prints the preview of `check`. It writes no entry and
  fetches no file. A folder that is already configured (same `key`) is updated, not
  duplicated. It is a usage error for an account that is not Google or Microsoft, and for
  a URL that is not a folder of the account's provider.
- **`list`** shows every configured folder, with its coverage (`covered_since`) and the
  time of its last listing.
- **`remove`** deletes the setting and its `sync_state` row. Entries already created stay
  (ADR-0022 decision 3).
- **`check`** lists the folder(s) like a sync would, and prints, for the window of the next
  run (or of `--since`): files in scope, how many are new, changed or unchanged, files
  skipped as unsupported or excluded, the largest files, and the total size. It writes
  nothing, so it can be repeated. Use it with `sb sync --estimate` before the first sync of
  a large folder.
- All commands take `--json`: schema `sb.folder/v1` (`folders`, or `check` with `counts`).
  They do not take the sync lock (`add` and `remove` write the account config in one
  transaction; `check` writes nothing).

### Resolving the folder URL

- **Google** (`drive.google.com`, folder forms `/drive/folders/<id>`, with or without
  `/u/<n>/` and query; `open?id=<id>` and `/folders/<id>` of a shared drive): the ID is
  taken from the URL and checked with `files.get` (`mimeType = application/vnd.google-apps.folder`,
  `supportsAllDrives=true`). A shortcut is followed to its target. A file URL is a usage
  error ("pass a folder; use `sb ingest` for a file").
- **Microsoft** (the host rules of ingest.md): `GET /shares/u!{base64url(url)}/driveItem`
  (the same call as `locate`), which must return a `folder` facet. A `remoteItem` is
  followed to the target drive and item. A file is a usage error.
- The URL itself is never requested by `sb` (ADR-0021 decision 4).
- The account is chosen by the first argument. There is no probing: the user names the
  account.

## Listing

One listing per configured folder per `sb sync`, when the account's folder sync is
selected (below). Tokens, retries, `Retry-After` handling and the `needs_reauth` rules are
those of the account's other calls.

1. **Root.** Fetch the root's metadata (name, trashed). A root that is gone or no longer
   readable ends the folder's sync with the issue `folder.unavailable`, and leaves the
   entries as they are.
2. **Walk** the tree breadth first. For each folder, list its children page by page:
   - Google: `files.list` with `q = '<id>' in parents and trashed = false`,
     `supportsAllDrives=true`, `includeItemsFromAllDrives=true`, `pageSize=1000`, the fields
     `id, name, mimeType, modifiedTime, createdTime, size, webViewLink, shortcutDetails`;
   - Microsoft: `GET /drives/{driveId}/items/{itemId}/children?$top=200` with
     `$select=id,name,size,eTag,cTag,createdDateTime,lastModifiedDateTime,webUrl,file,folder,package,deleted,remoteItem,parentReference`,
     following `@odata.nextLink`.
3. **Folders** are entered when `recursive` is true and the depth is within `max_depth`. A
   visited-set of folder IDs stops cycles. A shortcut (Google) or `remoteItem` (Microsoft)
   that points to a **folder** is not followed.
4. **Files** are judged by the rules of the source (`google.doc` / `microsoft.file` type
   tables). Unsupported types (including Google forms, drawings, sites, OneNote
   `package`, images, audio, video, archives) are counted as `unsupported` and skipped. A
   shortcut or `remoteItem` that points to a **file** is followed, and the entry is keyed
   by the target (as for ingest).
5. **Filters.** `include` / `exclude` and the built-in excludes. A file that fails them is
   counted as `excluded`.
6. **Scope.** A file is in scope when `modifiedTime >= covered_since` of the folder (below).
7. **Compare.** For an in-scope file, look up the entry by natural key (`SyncHost::fetch_state`):
   - no entry: `folder_new`;
   - entry whose stored `modified_time` (Google) or `etag` (Microsoft) differs:
     `folder_changed`;
   - equal: unchanged, nothing to do;
   - a Drive file ID that exists as a `google.meet` entry: `duplicate`, nothing to do
     (ingest.md).
8. **Enqueue** all `folder_new` and `folder_changed` files as `QueueItem`s with
   `reason = folder_new | folder_changed` and
   `hint = {"extra": {"sync_folder": <key>, "folder_path": "<path>"}, "modified_time": "...",
   "etag": "...", "name": "...", "size": n}` (shaped like `IngestHint`, so the fetch picks up
   `extra`; `name` and `size` are for `sb folder check`),
   together with the folder's updated coverage, in one transaction per 500 items
   (ADR-0012). An existing queue row for the same key is replaced.
9. **Stop conditions.** If the files in scope exceed `max_files`, nothing is enqueued for
   this folder and the issue `folder.too_large` (warning) says how many files and which
   limits to raise. The cancellation token is checked between pages.

The queue is drained by the usual `drain_queue`: each item is fetched with the source's
`fetch`, passing the hint, and committed in chunks. A failed item stays in the queue with
its attempts (ADR-0012) and is replaced by the next listing. Fetching an item whose
stored state already equals the listed state returns `Unchanged`, so a repeated listing
costs no download.

### Folder path

`folder_path` is the path of the file's **folder** from the configured root's own name
down, joined with ` / `, for example `Minutes / 2026 / Q3`. Names come from the listing of
this run; a path that would exceed 300 characters keeps the first segment and the last
segments with `...` between. The path is passed to the summarizer as `location`
(ADR-0023). It is not a key and is not used for identity.

### Coverage (ADR-0016)

`sync_state` key `folder:<key>` (source kind of the adapter's folder kind) holds
`{"covered_since": "...", "listed_at": "..."}`.

- First run: `covered_since = run_started_at - initial_days`, where `run_started_at` is
  the run's start. Only files modified at or after it are in scope.
- `sb sync --since X`: for each folder with `covered_since > X`, the window becomes `X`
  (all files modified since X are in scope) and `covered_since` is set to `X` in the same
  transaction as the enqueue. A folder already covered back to `X` is not listed again
  for this purpose.
- `sb sync --since X --until Y`: files with `modifiedTime` in `[X, Y)` are compared and
  enqueued whatever the coverage says (repair). Coverage is updated only if `Y >= covered_since`.
- `--until` without `--since` is a usage error (cli.md).
- Invariant: every supported, non-excluded file of the folder with `modifiedTime >=
  covered_since` is an entry, is in `sync_queue`, or is a `google.meet` duplicate.
  `covered_since` is written in the same transaction as the queue rows it covers.

A file modified after it was out of scope enters the scope by its new `modifiedTime`. A
file that was in scope and is later moved out of the folder is not touched.

## When folder sync runs

- `GoogleSource::sync` runs its Meet sync (if the account has the Meet source enabled) and,
  when the account has folders, the folder sync. A Google account with the `docs` feature
  only has no Meet sync and still syncs folders.
- `MicrosoftSource::supports_sync()` is true when the account has at least one folder.
  Otherwise the account is reported as "no sync configured", not as an error.
- `sb sync --source google.doc` or `--source microsoft.file` runs only the folder sync of
  those accounts. `--source google.meet` and `--source slack` do not run it.
- `--deep` has no extra effect on folders.
- `sb sync --dry-run` lists the queued items and does not list folders (no network).
- Provider access is one trait, `FolderOps` (`sb-kernel`): listing, change detection, the kinds
  that make an entry a duplicate, and URL resolution. A source returns it from
  `Source::folders()`. `sb folder` and the sync use the same code, so `check` shows what a
  sync would do.

## What is stored

Entries are those of `sb ingest` (ingest.md "What is stored") with:

- `origin = sync`;
- fetch metadata keys `sync_folder` (the folder `key`) and `folder_path`;
- `summaries` with `document-summary/v3` (ADR-0023), and no length threshold by default
  (summarization.md).

If the file already exists as an entry made by `sb ingest` or by a link, its origin is
kept, and the folder keys are added to its metadata at the next fetch.

## Stats and JSON

A sync reports what it queued and fetched under `stats.sources` like any source
(`<account>/google.doc`: `queued`, `new`, `updated`, ...), and its folder problems as errors
and issues. The per-folder breakdown (`listed`, `in_scope`, `new`, `changed`, `unchanged`,
`duplicate`, `unsupported`, `excluded`, `too_deep`, `status`) is the `FolderReport` that
`sb folder check` prints and returns in `--json` (`sb.folder/v1`, key `check`); it is not
part of `sb.sync/v1`.

## Errors and issues

| Code | Severity | When |
|---|---|---|
| `folder.unavailable` | error | The root cannot be read (deleted, access lost). Opened per account and folder; resolved by the next successful listing |
| `folder.too_large` | warning | Files in scope above `max_files` |
| `folder.failed` | warning | Listing the folder failed for another reason (an API error, a next-page link that leaves the provider's host). The other folders are still synced |
| `auth.needs_reauth` | error | As for every account |

A single file that cannot be fetched is an ordinary failed queue item (`sync.queue` check
of doctor), not a folder issue.

## Security

- The access tokens go only to Google and Microsoft endpoints, as before. Folder and file
  IDs come from the API responses.
- A folder is named by the user. Subfolder and file names come from the provider and are
  used as text only (the path hint, the title). They never become paths on the local file
  system.
- `folder_path` and file names are untrusted data in the prompt (ADR-0023).
- Logging: names of folders and files are logged only at `debug`; counts at `info`.

## Tests

- `wiremock` for Drive (`files.list` pages, shared drive flags, shortcuts to a file and to a
  folder, a cycle, trashed items, `modifiedTime` windows) and Graph (`children` paging,
  `remoteItem`, `package`, `deleted`, `folder` and `file` facets, `403` on the root).
- Coverage: first run, `--since` extension, `--since/--until` repair, a file that enters the
  scope later, an interruption between the enqueue and the drain (the resumed run equals
  an uninterrupted one, ADR-0012).
- Filters: include, exclude, built-in excludes, depth limit, `max_files`.
- Duplicates: a Drive file ID present as `google.meet`.
- `sb folder add` / `check` / `remove` with a temporary `SECOND_BRAIN_HOME`; URL forms for
  both providers; a file URL is a usage error.
- Fixtures contain no real names or IDs.
