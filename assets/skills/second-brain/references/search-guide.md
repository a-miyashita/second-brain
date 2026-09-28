# Search guide

## Which section to search

| Question type | Section to try first |
|---|---|
| "What was decided about X?" | `decisions`. Agreed outcomes are often **only** there, not in `details` |
| "Why? What was the history?" | `details` |
| "Who owns this task?" | `action_items` |
| General overview | `overview` |
| Why a document was added | `background` |

`details` of Slack entries is the rendered conversation itself; `details` of
meetings is the discussion summary.

## Query tips

- Terms are ANDed and matched as substrings (case-insensitive for Latin text).
  Full-width and half-width forms are normalized (ＣＳＶ = CSV).
- Terms of one or two characters, common in Japanese (契約, 要件, 納期), are
  matched too; with only short terms, results are ordered newest first.
- Prefer specific nouns: customer names, product and feature names, ticket IDs,
  people's names. Avoid generic words such as "meeting" or "decision".
- Narrow with `--source`, `--account`, `--since` and `--until` when the user
  mentions a channel type, a workspace or a period.
- `--limit` defaults to 8 (maximum 50). Increase it only when you need to scan
  broadly, then read just the relevant hits.

## Output fields (`sb.search/v1`)

Each hit has `entry_uid`, `title`, `source_kind`, `account` (`id`, `label`),
`date`, `section`, `snippet`, `score` and `cite_url`. `cite_url` is the original
link (Slack permalink, Google Doc), falling back to a transcript link or the local
raw file path.

## Listing and browsing

- `sb list --json --source google.meet --since 2026-09-01 --limit 20` lists
  entries newest first (useful for "what meetings did I have last week").
- `sb stats --json` shows what the knowledge base contains.
