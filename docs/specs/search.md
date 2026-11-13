# Search

Related ADRs: 0004, 0024.

## Query model

```text
SearchQuery {
  terms: Vec<String>,          // AND semantics
  sections: Vec<SectionKind>,  // empty = all
  source_kinds, accounts,      // empty = all
  since, until,                // on source_created_at (fallback ingested_at)
  limit: u32 (default 8, max 50),
  mode: Auto | FullText | Vector | Hybrid   // only FullText in MVP
}
```

The result is a list of `Hit`:

- `entry_uid`, `section`, `score`, `snippet`;
- entry summary fields: `title`, `source_kind`, account, date, `cite_url`.

A hit is one entry and section. Entries are deduplicated to their best-scoring
section unless `--all-sections` is given.

## `sqlite-fts` backend (MVP)

- **Indexing:** one FTS row per section. The row holds `title`, repeated in every
  row so title matches score, and `text`.
- **Terms of ≥ 3 characters:** combined as an FTS5 `MATCH` of quoted phrases joined
  with `AND`, ranked with `bm25(fts_sections, 2.0, 1.0)` (title weighted).
- **Terms of < 3 characters:** these include most two-character Japanese words, and
  trigram FTS returns **no rows** for them without any error.
  - They are matched with `sections.text LIKE '%' || ? || '%' ESCAPE '\'`, or the
    title.
  - If all terms are short, results are ordered by date, newest first, with score 0.
  - If terms are mixed, FTS hits are intersected with the LIKE matches.
- **Snippet:** FTS5 `snippet()` for FTS hits, or a manual window of about 80
  characters around the first match for LIKE hits.
- **Filters:** applied by joining `entries`.
- **Unicode:** the query is NFKC-normalized. FTS5 trigram is case-insensitive
  (`case_sensitive 0`).

## Section guidance (for skill and MCP descriptions)

| Question type | Section to try first |
|---|---|
| "What was decided about X?" | `decisions`. Agreed outcomes are often **only** there, not in `details` |
| "Why? What was the history?" | `details` |
| "Who owns this task?" | `action_items` |
| General overview | `overview` |
| Why a document was added | `background` |

## Questions without a keyword

Search needs a term. "What are my TODOs today?", "What happened yesterday?" and "What did
I do this year?" have none, and a term such as "TODO" matches only the entries that
happen to contain it. Those questions are answered by period and kind, not by terms
(ADR-0024):

| Question type | Use |
|---|---|
| "What happened today / this week?" | `sb recent --since today` (or `7d`) |
| "My TODOs", "what is due", "what am I waiting for" | `sb items --kind action --status open --mine` (`--due-by`, `--asked-by-me`) |
| "What did I do / finish in the last N months?" | `sb items --status done --mine --since Nd`, with kinds `accomplishment`, `resolved`, `action` |
| "What is still unanswered?" | `sb items --kind question --status open` |
| A topic or a named thing | `sb search` |

The `decisions` and `action_items` sections that search returns are rendered from the
same items, so `sb search --section action_items` and `sb items --kind action` agree.
Details: [items.md](items.md).

## Future: vectors and hybrid search

- `sqlite-vec` virtual table keyed by `sections.id`. Long sections are chunked,
  with a chunk table mapping `chunk_id → section_id, offset`.
- An `embeddings` table records the embedder and model per chunk and the section
  hash, so changed sections are re-embedded.
- Hybrid mode runs FTS and vector search and merges them with Reciprocal Rank
  Fusion (k = 60).
