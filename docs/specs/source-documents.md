# Sources: Google Docs, web pages and local files

Related ADRs: 0002, 0006, 0008, 0014.

Three on-demand sources (no `sync`) used by `sb ingest` ([ingest.md](ingest.md)). They
implement `Source` with `supports_sync() = false`; `fetch` and `normalize` do the work.
`fetch` gets the original bytes, runs `sb-extract` ([extract.md](extract.md)) and returns
the extracted Markdown as the raw data (ADR-0014). `normalize` is pure: it turns the stored
text and the fetch metadata into sections.

| Kind | Crate | Account |
|---|---|---|
| `google.doc` | `sb-google` | a Google account |
| `web.page` | `sb-ondemand` (new) | pseudo-account `web` |
| `local.file` | `sb-ondemand` (new) | pseudo-account `local` |

Common to all three:

- **Raw data** (ADR-0014): one `extracted_text` object (segment 0, `text/markdown`, ext `md`),
  cut at `ingest.max_text_chars`. `raw_status = present` requires it. The original bytes
  are stored as a `primary` object (segment 0, ext by type) only with `--keep-original` /
  `ingest.keep_original`. Otherwise they are dropped after extraction, and their
  `original_sha256` and `original_size` go to the fetch metadata. Both objects belong to a
  `Replace` bundle.
- Every source returns `NotApplicable(reason)` when the item is not something to make
  an entry from, and `NotFound(reason)` when it is gone or inaccessible.
- Common fetch-metadata keys: `context`, `title_override`, `date_override` (RFC 3339; from
  the CLI), `original_sha256`, `original_size`, and the extractor results `extractor`
  (for example `"sb-extract 0.1/pdf"`), `doc_title`, `doc_created`, `doc_modified`,
  `page_count`, `sheet_names`, `slide_count`, `text_truncated`, `extract_warnings`. `normalize` applies them: the title and
  `source_created_at` are replaced, the context becomes the `background` section and
  `SummaryInput.context`.
- The title falls back to: `doc_title` (the extractor title), then the file or page name,
  then the locator.
- Sections, summary and thresholds: see ingest.md "What is stored".
- Extracted text below `ingest.min_text_chars` makes `fetch` return
  `NotApplicable("no extractable text ...")`, so such items are dropped before any raw
  data is written.
- `sb refetch` runs the same `fetch`. `sb reextract` only re-runs `normalize` on the stored
  text, so a better extractor reaches old entries through `sb refetch`, while the source
  exists.

## `google.doc`

### Identity

- `source_id`: the Drive **file ID**.
- `source_url`: the file's `webViewLink`.
- `source_created_at`: `createdTime`; `source_updated_at`: `modifiedTime`.
- The same ID as a `google.meet` entry is a duplicate (ingest.md). Imported
  `google.doc` entries use the same ID, so they match.

### Fetch

1. Metadata: `files.get` with the fields `id, name, mimeType, createdTime, modifiedTime,
   webViewLink, owners(displayName, emailAddress), size, trashed, shortcutDetails`
   (`supportsAllDrives=true`).
   - `trashed` is `NotFound("in the trash")`.
   - A shortcut (`application/vnd.google-apps.shortcut`) is followed to
     `shortcutDetails.targetId`; the entry is keyed by the **target** ID.
2. Unchanged check: if the stored `fetch_state.modified_time` equals `modifiedTime` and
   `full = false`, return `Unchanged` without downloading.
3. Get the content by MIME type:

   | MIME type | How | Original ext (kept only with `--keep-original`) | Extracted as |
   |---|---|---|---|
   | `application/vnd.google-apps.document` | `files.export` as `text/markdown` | `md` | Markdown, with the Google cleanup below |
   | `application/vnd.google-apps.spreadsheet` | `files.export` as OOXML | `xlsx` | xlsx (every sheet) |
   | `application/vnd.google-apps.presentation` | `files.export` as OOXML | `pptx` | pptx (slides and speaker notes) |
   | `application/pdf`, docx, pptx, xlsx, xls, ods | `files.get?alt=media` | by type | the matching extractor |
   | `text/*`, `application/json`, Markdown, CSV | `files.get?alt=media` | by type | text |
   | folders, forms, drawings, sites, images, audio, video, archives, other | none | | `NotApplicable("unsupported Drive file type: <mime>")` |

4. Extract (below), then return a `Replace` bundle with `fetch_state = {"modified_time": ...}`
   and the fetch metadata (below).

Limits and errors:

- Drive refuses exports above 10 MB (`exportSizeLimitExceeded`): `failed` with that
  reason. `ingest.max_file_bytes` applies to downloads.
- `404` and `403` for a file are both "not found or no access" when probing accounts
  (ingest.md). A single account gets the more specific message.
- `401` / `invalid_grant` is the usual `needs_reauth` handling (accounts-and-auth.md);
  the probe moves on to the next account.
- Rate limits and transient network errors use the retry policy of the other Google
  sources.

### Google Markdown cleanup

The exported Markdown gets the **same mechanical cleanup** as Gemini notes
(source-google-meet.md "Noise removal"): timestamp anchors and over-escaping
(`\[ \] \- \_ \* \# \.`) are removed, and runs of blank lines are collapsed. The code is
shared with the Gemini parser (one function in `sb-google`). The feedback-survey
removal is **not** applied. The cleaned Markdown is the `extracted_text`; with
`--keep-original`, the unmodified export is kept as `primary`.

### Metadata

`drive_file_id`, `mime_type`, `owners: [{name, email?}]`, `drive_modified_time`,
`export_format` (`markdown`, `xlsx`, `pptx`, `original`), plus the common keys.

### Accounts

No new account feature is needed: `docs` and `meet` both grant `drive.readonly`
(accounts-and-auth.md). The features list is only the way to ask for that scope.

## `web.page`

### Identity

- `source_id`: the **normalized URL**: scheme and host lowercased, IDNA host in
  punycode, fragment removed, default port removed, an empty path becomes `/`, and
  the parameters `utm_*`, `fbclid`, `gclid`, `mc_cid`, `mc_eid` removed. Other
  parameters stay in their order. `<link rel=canonical>` is stored in metadata but does
  **not** change the identity, since sites get it wrong.
- `source_url`: the URL after redirects, normalized in the same way.
- `source_created_at`: the page's own publication date if it has one (see below);
  `source_updated_at`: its modification date or the `Last-Modified` header. Both are
  `NULL` when unknown: `normalize` never reads the clock.

### Fetch

- Client: `reqwest` with `rustls`; gzip, deflate and brotli decoding (pure Rust);
  **no cookies and no credentials**; `User-Agent: second-brain/<version>`;
  `Accept: text/html,application/xhtml+xml,application/pdf,text/plain,text/markdown;q=0.9,*/*;q=0.1`.
- Redirects: followed up to `ingest.web.max_redirects` (5), only to `http`/`https`.
- **Non-public addresses are refused** (SSRF guard) unless `ingest.web.allow_private`
  is true:
  - refused: loopback, unspecified, private (RFC 1918), link-local
    (`169.254.0.0/16`, `fe80::/10`, which includes cloud metadata endpoints), carrier-grade NAT
    (`100.64.0.0/10`), unique-local (`fc00::/7`), multicast, and the IPv6 forms that
    embed an IPv4 address: IPv4-mapped, IPv4-compatible, NAT64 (`64:ff9b::/96`,
    `64:ff9b:1::/48`), Teredo (`2001::/32`) and 6to4 (`2002::/16`, by the embedded address);
  - it is enforced in the **DNS resolver** used by the client, so the check applies to
    the address actually connected to, on every redirect hop, and DNS rebinding cannot
    switch it. An IP literal in the URL is checked before connecting;
  - **proxies:** a proxy resolves names itself, so the resolver guard would never run.
    While the guard is on (`allow_private = false`) the client therefore uses **no proxy**,
    and when the environment configures one (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`)
    the fetch is refused with a message that says why. Setting
    `ingest.web.allow_private = true` turns the guard off and lets the proxy be used.
- Size: the body is streamed and the fetch is aborted above `ingest.max_file_bytes`.
- Status handling:

  | Response | Result |
  |---|---|
  | 2xx | continue |
  | 401, 403 | `failed`: "the page needs a login; save it as a file and ingest that" |
  | 404, 410 | `NotFound` |
  | 429, 5xx, network error | retried with backoff (the retry policy of other sources), then `failed` |
  | final host `accounts.google.com` (a Google login redirect) | `failed`, same hint as 401 |

- Content type (from `Content-Type`, sniffed by magic bytes when it is missing or
  `application/octet-stream`): HTML, PDF, docx, pptx, xlsx, `text/plain`, `text/markdown`,
  CSV are accepted. Anything else is `NotApplicable("unsupported content type")`.
- Conditional refetch: `fetch_state = {"etag", "last_modified", "sha256"}`. A re-ingest
  sends `If-None-Match` / `If-Modified-Since`; `304`, or an equal body hash, is
  `Unchanged`.
- Linked images, scripts and styles are **not** stored. The body is extracted at fetch
  time; it is kept (`html`, `pdf`, ...) only with `--keep-original`.

### Extraction

HTML goes through the main-content extractor of `sb-extract` (extract.md): navigation,
headers, footers, sidebars, scripts and forms are dropped; headings, lists, tables and
links are kept as Markdown. The title is `og:title`, then `<title>`, then the first
`<h1>`. The publication date is the first of `article:published_time`, JSON-LD
`datePublished`, or the first `<time datetime>` inside the main content.

Pages whose text is produced by JavaScript yield little or no text and end as
`NotApplicable("no extractable text; the page may need JavaScript")`.

### Metadata

`fetched_url` (as given), `final_url`, `canonical_url?`, `site_name?`, `content_type`,
`http_last_modified?`, `http_etag?`, plus the common keys.

### Logging

URLs are logged as `scheme://host/path` without the query string (it may carry a token).
Bodies are logged only at `trace`.

## `local.file`

### Identity

- `source_id` and `source_url`: the canonical absolute path as a `file://` URL
  (symlinks resolved; on Windows the real letter case and no `\\?\` prefix; path
  separators as `/`). A moved or renamed file is therefore a **new entry**; the
  duplicate check by content hash tells the user when that happens.
- `source_created_at`: the document's own creation time when the format records it
  (docx, pptx, xlsx, PDF metadata); otherwise the file's creation time where the OS
  provides one; otherwise the modification time. `source_updated_at`: the file's
  modification time. Both come from `fetch` and are passed in the fetch metadata, so
  `normalize` stays pure.

### Fetch

- The file is read once into memory (limit `ingest.max_file_bytes`) and hashed.
- **Refused paths** (`failed` with the reason `path is not allowed`; no override):
  - `$SECOND_BRAIN_HOME` and everything below it (it holds the credentials DB);
  - credential locations under the user's home: `~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.azure`,
    `~/.kube`, `~/.config/gcloud`, `~/.config/gh`, `~/.config/git/credentials`,
    `~/.docker/config.json`, `~/.netrc`, `~/.npmrc`, `~/.pypirc`, `~/.git-credentials`,
    `~/.password-store`, `~/.local/share/keyrings`, `~/Library/Keychains`,
    `~/.terraform.d/credentials.tfrc.json`, and on Windows `%APPDATA%\gcloud` and `\gh`;
  - secret files anywhere, by name: `.env`, `.env.*`, `*.pem`, `*.key`, `*.p12`, `*.pfx`,
    `id_rsa*`, `id_dsa*`, `id_ecdsa*`, `id_ed25519*`, `.netrc`, `_netrc`, `.npmrc`, `.pypirc`,
    `.git-credentials`;
  - any pattern in `ingest.local.deny` (globs, matched on the canonical path). `**/` in a
    pattern starts at a path segment, so `**/.env.*` does not match `notes.env.md`.

  The check runs on the **canonical** path, and the denied locations are checked in their
  canonical form too, so a symlink into a denied place is refused, and so is a denied
  directory that is itself a symlink. A dry run reports a denied path as `failed`.
- Supported formats: by extension, confirmed by content: `txt`, `md`, `markdown`, `csv`,
  `tsv`, `html`, `htm`, `docx`, `pptx`, `xlsx`, `xls`, `ods`, `pdf`. A file with an
  unknown extension is accepted as text if it decodes as text (no NUL bytes); otherwise
  `NotApplicable("unsupported file type")`. Legacy `.doc` and `.ppt` are
  `NotApplicable` with the hint "save it as docx / pptx".
- With `--keep-original` the bytes are copied to `primary.0.<ext>` (the extension taken from
  the file name when it is 1 to 8 ASCII alphanumerics, otherwise `bin`). Without it, only
  the extracted text is kept. Either way, later changes or deletion of the original file
  do not affect the entry.
- `fetch_state = {"mtime", "size", "sha256"}`. Re-ingesting an unchanged file is
  `Unchanged`; a file whose `original_sha256` equals that of another `local.file` entry
  (a copy under another path, or a moved file) is a `duplicate`.
- `sb refetch` re-reads the path. If the file is gone, `NotFound`: the entry and its
  stored text stay as they are.

### Metadata

`path` (the canonical path in the OS notation), `file_name`, `size`, `media_type`,
`file_mtime`, plus the common keys.

## Tests

- Drive: `wiremock` serving metadata, exports, downloads, shortcuts, trashed files,
  `403`/`404` probes and the export-size error.
- Web: `wiremock` on loopback with `ingest.web.allow_private = true`; the address
  guard has its own unit tests on the address classes and on the resolver. Cases: redirect
  loops, oversized body, `401`, `304`, content-type sniffing, JavaScript-only page.
- Local: temporary directories, symlink into a denied path, moved file (duplicate by
  hash), edited file (update), Shift_JIS text, unknown extension.
- `keep_original` on and off: the raw objects present, `original_sha256` recorded either
  way, and the unchanged and duplicate checks working without the original.
- Fixtures contain no real personal data. Binary fixtures (docx, pptx, xlsx, PDF) are
  tiny files generated by test helpers or written by hand and committed.
