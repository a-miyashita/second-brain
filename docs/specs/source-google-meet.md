# Source: Google Meet (`google.meet`)

Related ADRs: 0002, 0005, 0006, 0008.

## What an entry is

One entry per **Gemini notes document**, the Google Doc produced by "Take notes for
me". `source_id` is the notes document's **Drive file ID**. Both discovery
strategies find the same file ID, so deduplication is automatic. It also lets
imported entries match later syncs (see import-format.md).

- `source_url`: `https://docs.google.com/document/d/<id>/edit`.
- `source_created_at`: the meeting start time (from Calendar or Meet), falling back
  to the doc's `createdTime`.
- `source_updated_at`: the doc's `modifiedTime`.

## Raw data

| Role | Content | Obtained by |
|---|---|---|
| `notes` | Gemini notes as Markdown, exactly as exported | Drive `files.export` with `text/markdown` |
| `transcript` | A **separate** transcript document as Markdown (optional) | Link inside the notes, the Meet API `transcripts` resource, or a Calendar attachment |

**The transcript is usually embedded in the notes document itself.** It appears
after a top-level heading such as `# **📖 文字起こし**`, and in practice makes up
about three quarters of the file. Both layouts are supported:

- **Embedded:** the `notes` raw object stays as exported (raw data is never
  altered). `normalize` splits it at the transcript heading (`# **📖 …**` or
  `# **🎞 …**`): the part before is the notes, the part after is the transcript.
- **Separate:** when the notes only link to a transcript document, that document
  is fetched as a `transcript` raw object.

If both exist, the separate document wins. `has_transcript` in metadata records
whether any transcript was found.

`raw_status = present` requires the notes. A missing transcript is not an error.

## Discovery strategies

The setting `google.meet.strategies` (per account) takes an ordered list. Results are
unioned by file ID.

| Strategy | How | Covers | Phase |
|---|---|---|---|
| `calendar` | Calendar `events.list` over the last N days (default 3; `--since` for backfill). Skip cancelled events and events I declined. Take attachments whose MIME type is a Google Doc | Meetings I attended where notes are attached to the event, including notes stored in the organizer's Drive | M |
| `drive` | Drive `files.list` for Google Docs in the "Meet Recordings" folder (folder ID detected by name at setup, configurable), `modifiedTime > cursor` | Notes stored in my Drive | M |
| `meet_api` | Meet v2 `conferenceRecords.list` → `smartNotes.list` (state `FILE_GENERATED`) → `docsDestination.document`; `transcripts.list` for transcripts | To be verified: whether records of meetings I did not organize are visible | Spike S1; adopt if it covers the other two |

Documents that do not look like Gemini notes are rejected by `normalize` and
counted as "not applicable". A Gemini note is recognized by its section headings in
any supported locale. This happens, for example, with agenda docs attached to
events.

## Resumability (ADR-0012)

- Discovery (Calendar windows, Drive listing pages) enqueues notes file IDs into
  `sync_queue`, in the same transaction as the discovery cursor. The fetch stage
  drains the queue and commits in chunks.
- Notes and transcripts are rewritten documents, not appended ones. A changed
  `modifiedTime` triggers a **replace** (segment 0 only). An unchanged
  `modifiedTime` (stored in `fetch_state`) skips the export entirely.

## Normalization (Gemini notes parser)

This is a deterministic, heading-based parser. No LLM is involved; mechanical
processing has proven sufficient for Gemini notes and transcripts.

| Gemini heading (ja / en) | Section |
|---|---|
| 概要 / Summary | `overview` |
| 決定事項 / Decisions | `decisions` |
| 次のステップ / Suggested next steps | `action_items` |
| 詳細 / Details | `details` |

- All four sections get `origin = generated` and a `summaries` row with
  `source_native / google / gemini-meet-notes`.
- **Observed fact:** section headings are in **Japanese even for notes of meetings
  held in English**. Headings look like `### **概要**`; the notes title is a `##`
  heading. The parser accepts both the Japanese and the English headings anyway:
  whichever set appears is used, and seeing only one of them has no effect. Spike
  S2 re-checks the current format.
- A document without any of these headings (for example, an agenda doc attached to
  an event, or a transcript-only doc) is "not applicable" and produces no entry.
- **Noise removal** (deterministic, applied to every section):
  - Drop the feedback-survey line at the end ("これらのメモ…" / "How did we do…").
  - Replace in-document timestamp anchors `[00:12:34](#…)` with the bare time
    `00:12:34`. The anchors are useless outside Google Docs, but the time is
    useful.
  - Remove Google's over-escaping in Markdown export (`\[`, `\]`, `\-`, `\_`,
    `\*`, `\#`, `\.`).
  - Demote `##` sub-headings inside sections (e.g. 調整済み / さらなる議論が必要
    under 決定事項) to `###`.
  - Collapse runs of 3+ blank lines.
- **Transcript text** used as summarization input gets the same mechanical cleanup,
  with no LLM pre-processing.
- **Title:**
  1. The notes title (`##` heading), with Markdown escapes removed and any
     "(recurring)" suffix stripped.
  2. If Gemini auto-generated it because it could not determine the meeting name,
     fall back to the Calendar event title, then to the name of the notes'
     parent Drive folder (which is the meeting name) with its trailing date part
     removed. Auto-generated titles match patterns such as
     「2026／07／27 09：15 JST に開始した会議」, 「会議 2026年7月27日 09:15 JST」 or
     "Meeting started …" (with full- or half-width digits and separators).
- **Participants / absentees:** parsed from the 「招待済み」/ "Invited" line of the
  notes header, a list of `[name](mailto:email)` links.
  - A person whose link is struck through (`~~…~~`) was invited but absent and goes
    to `absentees`.
  - Everyone else goes to `participants`.
  - Calendar attendees are used only when the notes have no such line.
- **Links in the notes header:** a `calendar.google.com` link becomes
  `calendar_url`. A `docs.google.com` link becomes `transcript_url` (the transcript
  document).
- **Metadata:** `participants`, `absentees`, `recurring` (the event has
  `recurringEventId`, or the folder or title carries "(recurring)"),
  `calendar_url`, `transcript_url`, `has_transcript`.
- **Dates** come from API timestamps, not from the document text, whose date
  formats vary.

## Summarization

- Default profile for google.meet: `native`. The Gemini notes are kept and no LLM
  call is made.
- `sb resummarize --source google.meet --profile <P>`:
  - uses the transcript, with prompt `meeting-summary/v1`, which also produces
    `details`;
  - replaces the four generated sections;
  - uses map-reduce for long transcripts.
- If there is no transcript, the notes text is used and a warning is shown.
- `--native` restores Gemini's version from the raw notes.

## Refetch for imported entries

Imported meet entries carry the Drive file ID as `source_id`. `sb refetch --source
google.meet --raw-missing` exports the notes, and the transcript if it can be
located, and sets `raw_status = present` (or `fetch_failed` with the Drive error).
