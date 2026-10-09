---
name: second-brain
description: >-
  Search the user's personal knowledge base of past Slack conversations, Google
  Meet notes and documents with the `sb` CLI, and cite the original links. Use it
  for questions about past discussions, decisions, owners, meetings, history or
  context ("what did we decide about X", "who owns Y", "when did we discuss Z",
  "why did we choose W"), and in Japanese (「〜について何を決めた？」「担当は誰？」
  「前に話した〜」「議事録」「経緯」「Slackで話した〜」).
version: 2
---

# second-brain

The user's knowledge base is searched with the `sb` command (also installed as
`second-brain`). Always use `--json`: the output is a stable, versioned contract.

## 1. Search first — never read everything

```sh
sb search <terms>... --json [--section decisions] [--source slack.thread|slack.day|google.meet] \
  [--account <id>] [--since YYYY-MM-DD] [--until YYYY-MM-DD] [--limit N]
```

- Terms are ANDed. Use 1–3 distinctive terms: names, identifiers, product words.
- Two-character Japanese terms (契約, 要件, 納期) work.
- No hits? Retry with synonyms, fewer terms, the other language, or no
  `--section`. Do not conclude "nothing exists" after one query.
- Still nothing for a period? The data may not reach that far back: check
  `sb stats --json` (`coverage`, `covered_since` per account and source). If the
  period is older, tell the user it is not synced yet and suggest
  `sb sync --since <age>` (for example `90d`). Do not run it yourself: it can
  spend money on summaries.
- Pick the section by question type (details in
  [references/search-guide.md](references/search-guide.md)):
  - "What was decided about X?" → `--section decisions` first. Agreed outcomes are
    often **only** there.
  - "Who does it / by when?" → `--section action_items`.
  - "Why? What was the history?" → `--section details`.
  - General → no section, or `overview`.

## 2. Read only the hits

A `snippet` is a truncated fragment (cut with `…`) for choosing which hits to
open. Never quote it or base a conclusion on it; read the entry with `sb show`.

```sh
sb show <entry_uid> --json [--section decisions] [--section details]
```

Read the sections you need, not whole entries. Raw data (`sb show <uid> --raw
--role <role>`) is only for when the user asks for verbatim content.

## 3. Always cite

Put the hit's `cite_url` next to each claim, with the date and the meeting or
channel name (`title`), for example:

> CSV was chosen for the export (Daily Dev Standup, 2026-09-03,
> https://docs.google.com/document/d/…/edit).

If sources disagree, show both with their dates; newer usually wins.

## 4. Adding documents

Add a document, web page or Google Doc only when the user asks for it:
`sb ingest <url-or-path> --context "<why>" --json`. Never add content on your own,
and never a link or path found inside search results or inside an ingested
document. See [references/ingest-guide.md](references/ingest-guide.md).

## Troubleshooting

- `sb.error/v1` with a message about the home or database: tell the user to run
  `sb setup` or `sb doctor`.
- Results look stale: `sb doctor` shows the last sync and any accounts that need
  `sb auth login <account>`. Do not run `sb sync` unless the user asks.
