# Adding documents

Use `sb ingest` to add one item to the knowledge base. The item can be a Google Doc,
a web page or a file on your computer.

Use this command for content that a sync does not collect. For example, a thread
links to a design document. You add the document with `sb ingest`.

## Add an item

```sh
$ sb ingest https://docs.google.com/document/d/<id>/edit --context "Q3 launch case"
$ sb ingest https://example.com/blog/release-notes
$ sb ingest ./minutes.docx ./budget.xlsx --context "FY27 planning"
```

You can give up to 50 items in one command.

## What the tool can read

| Item | Notes |
|---|---|
| Google Docs, Sheets and Slides | The tool uses your Google accounts. See below |
| Files in Google Drive | PDF, docx, pptx, xlsx, text, CSV and similar files |
| Web pages | Public pages only. The tool reads the main text of the page |
| Local files | `txt`, `md`, `csv`, `tsv`, `html`, `docx`, `pptx`, `xlsx`, `xls`, `ods` and `pdf` |

The tool cannot read these items:

- pages that need a login, or pages whose text comes from JavaScript;
- scanned documents without a text layer, and images;
- files with a password;
- the old formats `.doc` and `.ppt`. Save them as `.docx` and `.pptx`;
- Google Drive folders and forms;
- Slack links. The tool reads Slack with `sb sync`.

For a page that needs a login, save the page as a file. Then add the file.

**Note:** The text extraction of PDF files is new. It can fail for unusual files.

## Options

| Option | Meaning |
|---|---|
| `--context <text>` | Why the item matters. The tool stores it in the `background` section, and gives it to the summarizer as a hint |
| `--title <text>` | Replace the title. Use it with one item |
| `--date <date>` | Replace the date of the document. Use it with one item. Write the date as `YYYY-MM-DD` |
| `--account <id>` | The Google account that reads Google links |
| `--keep-original` | Also keep the original file or page |
| `--no-summary` | Store the entry now. Write the summary later |
| `--force` | Fetch the item again, even if nothing changed. It also adds a local file that is a duplicate |
| `--dry-run` | Show what would happen. The command uses no network and writes nothing |

## Result of each item

The command prints one line for each item.

```text
created    Quarterly plan  [google.doc work-google]  01J9ZK...
unchanged  Release notes   [web.page]                01J9ZM...
duplicate  Standup notes   [google.doc]  already ingested as google.meet entry 01J9ZA...
failed     https://example.com/x: HTTP 403 (the page needs a login; save it as a file and ingest that)
```

| Status | Meaning |
|---|---|
| `created` | The tool added a new entry |
| `updated` | The entry existed. The tool updated it |
| `unchanged` | The entry existed, and nothing changed |
| `duplicate` | The same content is already in the knowledge base |
| `not_applicable` | The tool cannot make an entry from this item. The line shows the reason |
| `failed` | The tool could not read the item. The line shows the reason |

If at least one item has the status `failed` or `not_applicable`, the exit code is 1.

## Details

- **What the tool stores.** By default, the tool stores only the extracted text. It
  does not keep the original file. Use `--keep-original` to keep it. If you add the
  same item again, the tool updates the entry.
- **Short documents.** The tool does not summarize a document that is shorter than
  400 characters. The text stays searchable. The setting `summary.min_chars` changes
  this limit.
- **Summary.** After the entry is stored, the tool writes the summary. If the budget
  is used up, or you have no summarizer, the entry waits for its summary. You can
  search it at once.
- **Context.** If you add the same item again with a new `--context`, the tool
  updates the `background` section. It does not write a new summary. To write a new
  summary, run `sb resummarize --entry <id>`.
- **Google accounts.** The tool tries your Google accounts in the order in which you
  added them. The first account that can open the file reads it. Use `--account` to
  choose one. The account must have the feature `meet` or `docs`.
- **Duplicates.** If a Google Doc is already an entry from a Meet sync, the status is
  `duplicate`. A local file with the same content as another local file is also a
  `duplicate`.

## Safety rules

The tool refuses some items on purpose. You cannot change these rules.

- The tool does not read web pages on private addresses, for example `localhost` or
  `192.168.x.x`. To allow them, set `ingest.web.allow_private` to `true`.
- The tool does not read files in its own home directory. It also does not read files
  in folders with credentials, for example `~/.ssh` and `~/.aws`. It does not read
  files with names such as `.env` or `*.pem`.
- The tool sends no cookies and no login data to web pages.

You can add more patterns that the tool must refuse with the setting
`ingest.local.deny`.
