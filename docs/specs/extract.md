# Text extraction (`sb-extract`)

Related ADRs: 0002, 0008, 0014.

`sb-extract` turns the bytes of a document into Markdown text and a little metadata. It
is called by `fetch` of `google.doc`, `web.page` and `local.file`
([source-documents.md](source-documents.md)); the result is stored as the raw
`extracted_text` object (ADR-0014). Entries are not extracted again unless they are fetched again.

## Rules

- **Pure**: bytes in, result out. No file, network, environment or clock access, no
  randomness, no logging of content. The same input and the same crate version always
  give the same output, so a refetch of an unchanged source gives the same text.
- **Rust only**: no C libraries and no external programs, so that the same code builds
  on Windows, macOS and Linux and the binary stays a single file (ADR-0001).
- **Untrusted input**: every file may be hostile or broken. The rules under "Limits"
  apply to every format.
- Part of the crate is behind Cargo features (`html`, `docx`, `pptx`, `xlsx`, `pdf`) so
  that a format can be disabled. Release builds enable all of them.

## API

```text
detect(bytes, media_type?, file_name?) -> Format
extract(input: ExtractInput, limits: ExtractLimits) -> Result<Extracted, ExtractError>

ExtractInput  { bytes, media_type?, file_name? }
ExtractLimits { max_text_chars, max_decompressed_bytes, max_entries, max_pages, max_cells }
Extracted     { title?, text (Markdown), created?, modified?, stats, warnings }
ExtractError  { Unsupported(format), Corrupt(msg), Encrypted, TooLarge, Empty }
```

- `Format`: `text`, `markdown`, `csv`, `tsv`, `html`, `docx`, `pptx`, `xlsx`, `pdf`,
  `unknown`. Detection uses magic bytes first (`PK` zip with the OOXML part names,
  `%PDF-`), then the media type, then the file extension.
- `text` is already normalized: `\n` line endings, no BOM, no NUL, runs of 3 or more
  blank lines collapsed, trailing spaces removed.
- `created` / `modified` come from the document's own properties
  (docx/pptx/xlsx core properties, PDF info dictionary, HTML metadata).
- `stats` has `page_count?`, `sheet_names?`, `slide_count?`, `truncated` (the text was
  cut at `max_text_chars`).
- `warnings` is a short list of non-fatal notes (`"3 pages had no text layer"`).
- `Encrypted` (password-protected PDF or OOXML) and `Empty` (no text after extraction)
  are errors the source turns into `failed` / `NotApplicable` with a clear message.

## Formats

| Format | Output |
|---|---|
| Text / Markdown | Decoded text, unchanged otherwise |
| CSV / TSV | A Markdown table (the first row is the header). Quoted fields and embedded newlines are handled; newlines inside a cell become `<br>` |
| HTML | Main content as Markdown: headings, paragraphs, lists, tables, code blocks, links as `[text](url)`, image alt text. See below |
| docx | Headings (styles `Heading N` become `#`), paragraphs, lists, tables as Markdown tables, hyperlink text, footnotes at the end. Inserted tracked changes are kept, deleted ones dropped. Headers, footers, comments and images are skipped |
| pptx | One block per slide: `## Slide N: <title>`, then the text of the shapes in document order, then `Notes:` with the speaker notes |
| xlsx / xls / ods | One block per visible sheet: `## Sheet: <name>`, then a Markdown table of the used range. Formulas give their cached values. Hidden sheets are skipped. Bounded by `max_cells` |
| PDF | The text layer, paragraphs per page, pages separated by a blank line, no page markers. No OCR: a PDF without a text layer is `Empty` (a page-level count goes to `warnings`) |

Not supported: images, scanned documents, legacy `.doc` and `.ppt`, password-protected
files, archives, e-books. Each gives `Unsupported` or `Encrypted` with a hint.

### Character encodings

Text-like formats (text, Markdown, CSV, TSV, HTML) are decoded in this order:

1. a BOM;
2. for HTML, the `charset` of the `Content-Type` passed in the media type or of a
   `<meta>` tag;
3. strict UTF-8;
4. detection among Shift_JIS (CP932), EUC-JP and ISO-2022-JP, then Windows-1252.

Japanese files saved by Excel or old editors are often CP932, so this order matters.
The detected encoding goes to `warnings` when it is not UTF-8.

### HTML main-content extraction

A deterministic heuristic, in the spirit of readability, with no network and no
scripts:

1. Remove `script`, `style`, `noscript`, `template`, `iframe`, `svg`, `form`, `nav`,
   `header`, `footer`, `aside` and elements whose `role` is `navigation`, `banner`,
   `contentinfo` or `complementary`.
2. If there is an `<article>` or `<main>`, use it (the largest, if several).
3. Otherwise score block elements by the length of their text, the share of text inside
   links and the number of paragraphs, and take the best-scoring subtree.
4. Render that subtree as Markdown.

Pages that depend on JavaScript to produce their text give little or no text, which the
caller reports (source-documents.md).

## Limits

| Limit | Default | Applies to |
|---|---|---|
| `max_text_chars` | `ingest.max_text_chars` (300 000) | the output; stops the extraction early and sets `stats.truncated` |
| `max_decompressed_bytes` | 200 MiB | the sum of all zip members and compressed PDF streams (zip bombs) |
| `max_entries` | 10 000 | zip members and PDF objects visited |
| `max_pages` | 2 000 | PDF pages and pptx slides |
| `max_cells` | 1 000 000 | xlsx / csv cells |
| time | `ingest.extract_timeout_secs` (60) | the caller runs `extract` on a blocking thread with this deadline |

- `extract` is wrapped in `catch_unwind`; a panic inside a parser becomes
  `ExtractError::Corrupt`, not a crash of the command.
- A parser that loops forever cannot be stopped from inside the process. When the
  deadline passes, the caller abandons the thread and reports a failure; the process
  exits normally at the end of the command. If a chosen library proves unreliable in
  this respect (spike S6), the extraction moves to a short-lived child process of the
  same binary (`second-brain __extract`, stdin to stdout), which can be killed.

## Library choice

Chosen in spike **S6** (2026-10-06):

| Format | Library | Notes |
|---|---|---|
| zip containers (docx, pptx, ods) | `zip` + `quick-xml` | Own walkers for docx and pptx; sizes checked from the central directory |
| xlsx / xls / ods | `calamine` (feature `dates`) | |
| HTML | `scraper` (html5ever) | Own main-content scoring and Markdown renderer |
| CSV / TSV | `csv` | |
| Encodings | `encoding_rs` + `chardetng` | |
| PDF | `pdf-extract` 0.12 (on `lopdf`) | Per-page extraction (`extract_text_from_mem_by_pages`) |

PDF candidates tried: `pdf-extract`, `lopdf` (`Document::extract_text`) and `pdf_oxide`.

- `pdf_oxide` 0.3.78 did not compile (a dependency mismatch with `calamine`) and was dropped.
- Both `pdf-extract` and `lopdf` extracted a synthetic Japanese PDF (CID TrueType font,
  Identity-H) correctly and an English 37 000-character specification PDF in 10 to 50 ms,
  with no panic. `pdf-extract` keeps line breaks between paragraphs, `lopdf` joins them,
  so `pdf-extract` was chosen.
- **Not verified yet:** real-world Japanese PDFs (vertical writing, two columns, PDFs from
  Word, PowerPoint and Google Slides export) and a malformed-file corpus. The spike had no
  sample files without personal data. Until someone runs `sb ingest` on a few of their own
  PDFs and reports, PDF support is marked experimental in the README. A panic inside the
  library is caught and reported as a failure of that file.

## Tests

- Golden tests: fixture file in, expected Markdown out, per format. Fixtures are tiny
  generated or hand-written files with no personal data.
- Encoding tests: UTF-8, UTF-8 with BOM, CP932, EUC-JP, ISO-2022-JP.
- Limit tests: a zip bomb, a deeply nested document, a huge sheet, a truncated file,
  an encrypted PDF.
- Determinism test: extract twice, compare byte for byte.
