# Import bundle format (`second-brain-import/v1`)

Related ADRs: 0002, 0007.

Data from other tools is migrated by an exporter **outside this repository** that
writes this format. second-brain only knows this format, never the internals of
other tools.

## Bundle layout

```text
<bundle>/
├── manifest.json
├── entries.jsonl
└── raw/                     optional; files referenced from entries.jsonl
```

`manifest.json`:

```json
{"format": "second-brain-import/v1",
 "created_at": "2026-10-01T12:00:00Z",
 "producer": "example-exporter/1.0",
 "accounts": {"slack": "acme-slack", "google": "work-google"}}
```

`accounts` is an optional default mapping from account kind to account ID. CLI
`--map kind=account` overrides it. Target accounts must already exist in second-brain, so
the user runs `sb account add` first.

## `entries.jsonl` record

One JSON object per line:

```json
{
  "account": "work-google",
  "source_kind": "google.meet",
  "source_id": "1AbC...driveFileId",
  "source_url": "https://docs.google.com/document/d/1AbC.../edit",
  "title": "Daily Dev Standup",
  "source_created_at": "2026-09-03T00:15:00Z",
  "source_updated_at": null,
  "ingested_at": "2026-09-03T10:30:12Z",
  "metadata": {"participants": [{"name": "...", "email": "..."}],
               "import_ref": "2026-09-03_0915_Daily Dev Standup"},
  "sections": [
    {"kind": "overview", "origin": "generated", "text": "..."},
    {"kind": "decisions", "origin": "generated", "text": "- ..."},
    {"kind": "details", "origin": "generated", "text": "..."}
  ],
  "summary": {"generator_kind": "source_native", "provider": "google",
              "model": "gemini-meet-notes", "prompt_version": null,
              "generated_at": null},
  "raw": [{"role": "notes", "path": "raw/1AbC.md", "media_type": "text/markdown"}]
}
```

| Field | Required | Notes |
|---|---|---|
| `account` | no | Otherwise taken from the mapping by the kind implied by `source_kind` |
| `source_kind`, `source_id` | **yes** | Must follow the per-source spec, so later syncs and refetches match. Meet: Drive file ID of the notes. Slack thread: `<channel_id>:<thread_ts>`. Slack day: `<channel_id>:day:<date>`. Drive docs: file ID. Local file: absolute path (hashed by the importer if needed) |
| `title` | **yes** | |
| `sections` | **yes** (≥1) | Keys use the English section kinds (`overview`, `decisions`, `action_items`, `details`, `background`) |
| `summary` | no | If absent while generated sections exist, the importer records `generator_kind = "unknown"` |
| `raw` | no | **If absent or empty, the entry gets `raw_status = missing`**. Paths are relative to the bundle and are copied into `raw/` |
| others | no | |

## Import semantics

- **Idempotent:** records are upserted by `(account, source_kind, source_id)`.
  Re-running the same bundle changes nothing.
- **Never downgrade.** If an entry already exists with `raw_status = present` (for
  example, because sync already fetched it), the import updates only fields the
  entry lacks: `ingested_at` if older, and `import_ref` in metadata. It never
  replaces raw data or sections.
- `origin` is set to `import`. `summary_status` is `done` if a summary or generated
  sections exist, and `none` otherwise.
- **Validation:** `--dry-run` parses everything and reports errors per line without
  writing. A bad line is reported and skipped; it does not abort the import.
- After the import, the importer prints how many entries have `raw_status =
  missing`, and suggests `sb refetch --raw-missing`.

## Guidance for exporters (non-normative)

- Derive `source_id` from the original link whenever possible:
  - the Drive file ID from a Google Docs URL;
  - `channel_id` and `thread_ts` from a Slack permalink.

  That way, later syncs and `sb refetch` match the imported entries instead of
  creating duplicates.
- Include raw files whenever the exporter still has them. For example, a Slack
  thread's messages as JSONL (`primary`), or an exported Gemini notes document
  (`notes`).
- Record the original summarizer in `summary` when known, for example `llm_api /
  anthropic / <model>`. It is then possible to select those entries later with
  `sb resummarize --where-model`.
- A user-written "why this was added" note maps to `background` with
  `origin: user`.
