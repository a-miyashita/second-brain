# ADR-0014: Single-item ingest for Google Docs, web pages and local files

- Status: Accepted
- Date: 2026-10-06

## Context

Sync covers Slack and Google Meet. Much of the context people need lives elsewhere:
a design document linked from a thread, a PDF received by mail, a web article. ADR-0008
reserved the kinds `google.doc`, `web.page` and `local.file` and the single-item
style of ingestion ("add this document", usually driven by an agent, with a "why this
matters" context), but nothing implements them.

Decisions are needed on:

- how a locator (URL or path) is routed to a source kind and an account;
- what identifies an entry, so that re-ingesting updates instead of duplicating;
- where text extraction lives and which formats it supports;
- what protects the user when an AI agent chooses the locator (prompt injection can
  make an agent ingest `~/.ssh/id_rsa` or `http://169.254.169.254/`).

## Decision

1. **One command, three sources.** `sb ingest <locator>...` routes each locator by
   its shape (see [ingest.md](../specs/ingest.md)):

   | Locator | Source kind | Account |
   |---|---|---|
   | Google Docs / Sheets / Slides / Drive file URL | `google.doc` | a Google account with `drive.readonly` |
   | other `http(s)` URL | `web.page` | pseudo-account `web` |
   | file path or `file://` URL | `local.file` | pseudo-account `local` |

2. **The shared pipeline is reused** (ADR-0008): `fetch → store raw → normalize →
   upsert → summarize → index`. Entries get `origin = ingest`. The run is recorded as
   `command = ingest`. The sync lock, graceful interruption (ADR-0012) and the
   summarization budget caps (ADR-0013) apply unchanged. Nothing new is added to the
   schema.

3. **Natural keys** (ADR-0002) make re-ingest an update:

   | Kind | `source_id` |
   |---|---|
   | `google.doc` | Drive file ID |
   | `web.page` | normalized URL (lowercase host, no fragment, no default port, tracking parameters removed) |
   | `local.file` | the canonical absolute path as a `file://` URL |

   A Drive file ID that already exists as a `google.meet` entry (a Gemini notes
   document) is reported as a duplicate and not ingested again. A local file whose
   content hash already exists as a `local.file` entry is reported as a duplicate.
   `--force` overrides the second case only.

4. **The raw data of an ingested document is its extracted text, not the original file.**
   All three sources can be fetched again (a Drive file, a URL, a path), and link rot is
   accepted. Keeping every PDF, OOXML export or HTML page would grow the home directory
   (not the database) by one to two orders of magnitude for no use: the commands that
   read raw data (`reextract`, `resummarize`, `show --raw`) need the text, not the
   binary. So:

   - `fetch` runs the extractor (`sb-extract`) and stores the result as one raw object of
     role `extracted_text` (Markdown, segment 0). This is what `normalize` reads and
     what `resummarize` summarizes;
   - the original bytes are stored as a second raw object of role `primary` **only** when
     `--keep-original` or `ingest.keep_original = true` is given. By default only their
     SHA-256 and size are recorded (in the metadata), which is enough for the
     "unchanged" check and for finding duplicates;
   - a native Google Doc is already Markdown: its export, after the mechanical Google
     cleanup, is the extracted text.

   This amends the wording of ADR-0002 ("exactly what was fetched") for these sources:
   the raw data is the source content in the smallest form that search and
   summarization can use. See the Amendments of ADR-0002.

5. **`sb-extract` is pure and Rust-only**: bytes in, Markdown and metadata out. It has
   no I/O, clock or network access. It is called by `fetch`. It supports
   text, Markdown, CSV/TSV, HTML, docx, pptx, xlsx/xls/ods and PDF (text layer only).
   It decodes Shift_JIS/EUC-JP text, which is common in Japanese files. It has no OCR
   and no legacy `.doc`/`.ppt`. Extraction is bounded in size, in decompressed size and
   in time.

6. **User-supplied facts travel with the fetch metadata.** `--context`, `--title` and
   `--date` are stored in the entry's fetch metadata and applied by `normalize`:
   the context becomes the `background` section (`origin = user`) and the hint for the
   summarizer. Hence `sb reextract` and `sb refetch` keep them.

7. **Agents may choose the locator, so ingest is defensive by default**:
   - web fetches refuse loopback, private, link-local and other non-public addresses
     (checked on every redirect hop and on the address actually connected to), unless
     `ingest.web.allow_private` is set;
   - local files under `$SECOND_BRAIN_HOME` and a fixed list of credential locations
     (`~/.ssh`, `~/.aws`, `~/.gnupg`, cloud CLI config directories) are refused, with
     no override flag. The user can add deny patterns with `ingest.local.deny`;
   - downloads and archives have size limits; the page body is never logged above
     `trace`; URLs are logged without their query string;
   - the document prompt treats the body as untrusted data.

8. **No crawling.** One locator is one entry. Directories, Drive folders, sitemaps and
   link following are out of scope. `scan-links` (phase 2) stays a separate, read-only
   discovery command and never ingests by itself.

## Consequences

- A Google account can now supply two adapters (`google.meet`, `google.doc`).
  `SourceFactory` returns the adapters of an account instead of one.
- The skill can offer "add this document" once `sb ingest` ships. Its guidance
  (ask at most one question, never ingest on its own) is already written.
- Improving an extractor does not update stored entries through `sb reextract` (which
  only re-runs `normalize` on the stored text). `sb refetch` is needed, and it works
  only while the source still exists. That is accepted.
- Pages that need a login, or that render their text with JavaScript, cannot be
  ingested. The error says so, and saving the page as a file and ingesting that is the
  workaround.
- Summaries of ingested content cost money like any other. A batch of large PDFs can
  hit the weekly budget; the entries then stay `pending` and are summarized by
  later runs.
- PDF extraction quality depends on a pure-Rust library, which is unproven for Japanese
  PDFs. A spike (S6) picks the library before implementation. If none is good enough,
  PDF support moves to a later release instead of blocking the others.

## Alternatives considered

- **Keep the original bytes always** (the first draft of this ADR): rejected. Disk use
  grows with the size of PDFs and HTML pages, and nothing reads the originals. The
  option `--keep-original` remains for people who want them.
- **Keep originals only for `local.file`**: considered. A local file can disappear, but
  it usually stays on the disk it was ingested from, and the extracted text is enough
  for search and summaries. One rule for all three kinds is simpler.
- **`local.file` keyed by content hash**: an edited file would become a new entry and
  the old text would stay searchable next to the new one. Path keys make an edit an
  update. A moved file is a new entry; the duplicate check by hash catches an
  unchanged move.
- **Headless-browser rendering for web pages**: a large binary dependency and a bigger
  attack surface for little gain at personal scale. Rejected for now.
- **Sheets via CSV export**: Drive exports only the first sheet as CSV. The OOXML export
  keeps every sheet, and one extractor then serves Drive and local files.
- **Allowing `--allow-sensitive` for local files**: an agent can pass any flag. The deny
  list has no override; the user's escape hatch is to copy the file elsewhere.

## Amendments

### 2026-10-06: details settled during implementation

- The Consequences say that `SourceFactory` returns the adapters of an account. It does not:
  a Google account has one adapter, `GoogleSource`, that holds the `google.meet` and
  `google.doc` sources and dispatches by source kind, so the `Source` trait and the
  pipeline did not change.
- `web.page` and `local.file` live in a new crate `sb-ondemand`.
- A `SourceError::Rejected` variant carries refusals whose message is shown to the user as it
  is (a denied path, a page behind a login, a file above the size limit).
- Documents are judged by length (`summary.min_chars`) like conversations are; before, only
  conversations had a length threshold. The summarization prompt for documents is
  `document-summary/v2`.
- The duplicate check of `local.file` runs when the entry is committed, one locator at a time.
- PDF extraction uses `pdf-extract`. It was checked against a synthetic Japanese PDF and an
  English specification, not against real-world Japanese PDFs; see extract.md.

### 2026-10-06: fixes after code review

- **Proxies.** The address guard sits in the DNS resolver, which a proxy bypasses. While
  the guard is on, no proxy is used, and a fetch is refused when the environment configures
  one. `ingest.web.allow_private = true` turns both off. (Decision 7 above.)
- **Denied paths** also cover credential files outside the folders listed there
  (`~/.netrc`, `~/.config/gh`, `.env` files, private keys, ...), are matched in canonical form
  even when the denied directory is a symlink, and are applied by `--dry-run`.
- **Source refusals** are decided without side effects (`Source::refusal`), which is what the
  dry run uses.
- The result status of a commit is taken from what the commit did, so an entry that
  produced no text is `not_applicable`, and a Drive shortcut whose target already exists is
  `updated`.
- A document shorter than `summary.min_chars` says so in the result.
