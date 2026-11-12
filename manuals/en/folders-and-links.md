# Folders and linked documents

`sb ingest` adds one document at a time. Two settings let `sb sync` bring in documents
by itself:

- **Synced folders.** You name a folder in Google Drive, SharePoint or OneDrive. Each
  sync adds the new and changed documents in it.
- **Linked documents.** People paste links to documents into Slack threads. Each sync
  can add the documents that the links point to.

Both are off until you turn them on. Both only read. The tool never changes a document
or a folder.

## Sync a folder

### Before you start

You need a Google account or a Microsoft account that can open the folder. You do not
need a new permission. See [Google account](google-account.md) and
[Microsoft account](microsoft-account.md).

### Add a folder

Copy the link of the folder in your browser. In SharePoint and OneDrive, use
"Copy link". Then run:

```sh
$ sb folder add work-google "https://drive.google.com/drive/folders/<id>"
$ sb folder add work-ms "https://contoso.sharepoint.com/:f:/s/team/<token>" --label Minutes
```

The command opens the folder, saves it in the settings of the account, and prints
how many documents the next sync would add. It adds no document yet.

| Option | Meaning |
|---|---|
| `--label <text>` | A name for the folder. The default is the name of the folder |
| `--no-recursive` | Do not read subfolders |
| `--max-depth <n>` | How many levels of subfolders the tool reads. The default is 8 |
| `--include <glob>` | Add only files whose name or path matches. You can repeat the option |
| `--exclude <glob>` | Skip files whose name or path matches. You can repeat the option |
| `--initial-days <n>` | How many days back the first sync reads (see below) |
| `--max-files <n>` | Stop the folder if more files are in scope. The default is 5000 |

A glob uses `*` for any text inside one folder level, `**` for any text including
folder levels, and `?` for one character. Capital letters do not matter. Examples:
`*.docx`, `2026/**`, `_archive/**`. The tool always skips lock files such as `~$plan.docx`,
`Thumbs.db` and `.DS_Store`.

The command refuses a link to a file, a link from another service, and a folder that
the account cannot open.

### Look before you sync

A big folder can make many summaries, and summaries cost money. Look first:

```sh
$ sb folder check
$ sb folder check work-google Minutes --since 90d
```

`sb folder check` reads the folder like a sync, but it writes nothing. It prints how
many files are in scope, how many are new, changed or unchanged, how many it skips, and
the total size. Then run `sb sync --estimate` to see the cost of the summaries that are
waiting. See [Summaries and cost](summaries.md).

### Run the sync

```sh
$ sb sync
$ sb sync --source google.doc        # only the Drive folders
$ sb sync --source microsoft.file    # only the SharePoint and OneDrive folders
```

The first sync reads the files that someone changed in the last 30 days (the setting
`sync.initial_days`, or `--initial-days` for one folder). It looks at the **modification
date** of the file, not at the date when someone created it. To read older files, run:

```sh
$ sb sync --since 180d
```

The command reads only the files from the days that you did not read before. A later
sync adds a file again only if the file changed.

### What the tool adds

- The documents that `sb ingest` can read: Google Docs, Sheets and Slides, and PDF,
  Word, PowerPoint, Excel, text and CSV files. Other files are skipped without an error.
- A shortcut to a file is followed. A shortcut to a folder is not. In OneDrive, a folder
  that someone shared with you is not followed.
- A document that is already an entry is not added again. If the same Drive file is a
  Google Meet note, the tool keeps the Meet entry.

**Warning:** Do not add the folder that holds your Meet notes. The notes are collected
by the Meet sync. `sb doctor` tells you if a file is in both places.

When a file changes, the next sync reads it again. When you delete a file, move it out
of the folder or remove the folder from the list, the entry stays in the knowledge
base. This is the same as for all other sources. A file that you move to another
folder keeps its old folder name in the entry until the file changes.

### Folder names in summaries

The summarizer sees the path of the folder, for example `Minutes / 2026 / Q3`. It uses
the path only as a hint for reading the document. A folder called "Minutes" does not
make the summary say "These are meeting minutes". The summary takes the date, the names
and the topics from the text of the document.

### See and change the folders

| Command | Result |
|---|---|
| `sb folder list` | Shows the folders, how far back each one is read, and the time of the last listing |
| `sb folder check [<account> [<folder>]]` | Shows what the next sync would add. Writes nothing |
| `sb folder remove <account> <folder>` | Removes the folder from the list. The entries stay |
| `sb folder add <account> <link> ...` | Add the same link again with new options to change them |

The Folders section of `sb setup` shows the same list. Press Enter on a folder to run a
check, press `x` to remove it, or select "Add a folder". The other options are in the
settings of the account (`folders`). See [Settings](settings.md).

### Limits

If more than `max_files` files are in scope, the tool stops that folder and shows a
warning. It adds nothing from the folder until you fix this. Raise `max_files`, narrow
the folder with `--include` and `--exclude`, or reduce the days back.

If the folder cannot be opened any more, because someone deleted it or took your access,
the sync shows an error for that folder and goes on with the others. The error goes
away after the next sync that can open the folder.

## Documents linked in Slack

### Turn it on

The tool reads these links in the Slack threads that it syncs:

| Kind | Links |
|---|---|
| `google.doc` | Google Docs, Sheets, Slides and Drive files |
| `microsoft.file` | SharePoint and OneDrive files (`*.sharepoint.com`, `onedrive.live.com`, `1drv.ms`) |

The tool never reads other links: not web pages, not folders, and not Slack links.
By default it reads **none**. Choose the kinds that you want:

```sh
$ sb config set links.ingest_kinds '["google.doc", "microsoft.file"]'
```

For one workspace, set `ingest_links` in the settings of the account. This replaces the
global setting for that workspace. An empty list turns the feature off for it.

```sh
$ sb config edit --account acme-slack
```

You can also limit the links to some hosts. Set `ingest_link_hosts` to a list of host
names, for example `["contoso.sharepoint.com", "*.contoso.com"]`. If the list is empty,
all hosts of the chosen kinds are allowed.

If you choose a kind, but you have no account for it, nothing fails. The links wait.
`sb doctor` tells you how many links wait.

### What happens

After the Slack sync, the tool takes the links that it found and that you allowed. It
opens each one with your accounts, in the order in which you added them, and adds the
document as an entry. It reads at most 50 links in one sync (`links.max_per_run`). The
rest wait for the next sync.

- If no account can open a link, the tool tries again after 1, 3, 7, 14 and 30 days. After
  6 failed tries it stops. Use `sb scan-links --ingest --retry` to try again.
- If the link is a folder, a OneNote notebook or a file of an unsupported type, the tool
  marks it and does not try again.
- The tool reads a linked document one time. It does not read it again when the
  document changes. To update it, run `sb refetch --entry <id>`.
- The tool never opens the link itself. It asks Google or Microsoft for the file, with
  your account. Somebody who writes a link in Slack cannot make the tool open any other
  address.

### Why the thread matters

The thread tells the summarizer why people shared the document. The tool gives the
summarizer a short piece of the thread around the link, with the link itself replaced
by `[linked document]`. The summary then says why the document matters. The entry
stores the piece of text and the thread that it came from. Read them with
`sb show <id> --meta`.

**Note:** The piece of the thread goes to the same summarizer as the document. If you
use an LLM API, this text leaves your computer, as the thread text does when the tool
summarizes the thread itself.

### List the links

```sh
$ sb scan-links                       # all links in the synced Slack entries
$ sb scan-links --since 30d --kind microsoft.file
```

The command reads the saved entries and writes nothing. For each link it shows the
state:

| State | Meaning |
|---|---|
| `new` | The tool found the link in an old entry, but did not record it |
| `pending` | The link waits: the kind is off, you have no account for it, or the limit of the sync was reached |
| `ingested` | The document is an entry now |
| `unavailable` | No account can open it. The tool tries again later |
| `abandoned` | The tool tried several times and stopped |
| `not_applicable` | The link is not a document that the tool can read |
| `duplicate` | The document is already an entry from another source |

### Read links from threads that you synced before

The tool records links when it syncs a thread. For threads that you synced before you
turned the feature on, run:

```sh
$ sb scan-links --ingest
$ sb scan-links --ingest --kind google.doc --since 90d --limit 200
```

`--ingest` records the links of the entries and adds the documents. `--kind` allows a
kind for this one run, even if the settings do not allow it. The new entries wait for
their summaries: run `sb summarize` or the next `sb sync`.

## Summaries of these documents

- **Always summarized.** The tool writes a summary for every Google and Microsoft
  document, even a short one. The limits on cost still apply. When the budget is used up,
  the entry waits and stays searchable. To go back to the old rule, which skips short
  documents, run `sb config set summary.min_chars.google.doc 400`. Use
  `summary.min_chars.microsoft.file` for SharePoint and OneDrive.
- **No decisions in a document.** A document often has no decisions or action items.
  This is normal.
- **Older summaries.** Summaries that were made before this feature stay as they are.
  Run `sb resummarize` to write them again with the new prompt. See
  [Summaries and cost](summaries.md).

## Next step

Run `sb sync`, then read [Searching](search.md).
