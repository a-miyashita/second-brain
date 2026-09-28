# ADR-0004: Pluggable search backend; FTS5 trigram first, vectors later

- Status: Accepted
- Date: 2026-09-29

## Context

Search is the main way agents use the knowledge base. SQLite FTS5 with the
`trigram` tokenizer works for Japanese without a morphological analyzer. It has one
trap: queries shorter than three characters **silently return zero rows** through
FTS. Common Japanese two-character words (契約, 要件, 納期) hit this, so short terms
need a `LIKE` fallback on the section table.

Semantic search (vector / RAG) is wanted later. Its backends vary: sqlite-vec
inside the same DB, LanceDB embedded, Qdrant or pgvector as servers.

## Decision

- Define a `SearchBackend` trait in `sb-core`:
  - `index(entry, sections)`, `remove(entry_id)`, `rebuild(catalog)`,
    `search(query) -> Vec<Hit>`, and `capabilities()` (full-text / vector / hybrid).
  - `Hit` references `(entry_id, section_kind)` and carries a score and a snippet.
- The catalog notifies the active backend(s) whenever sections change. Backends own
  their tables and files and can always be rebuilt from the catalog.
- **MVP backend: `sqlite-fts`**
  - FTS5 `trigram` over section text and entry title, ranked with bm25.
  - Terms shorter than 3 characters are matched with `LIKE` against the sections
    table and intersected with the FTS results.
  - Multiple terms are ANDed.
- **Later: `sqlite-vec`**
  - The statically linkable `sqlite-vec` crate, loaded into the same connection.
  - Embeddings come from an `Embedder` trait (API or local, via an
    OpenAI-compatible endpoint).
  - Each embedding records its embedder and model, like summaries do (ADR-0005).
    Entries whose sections changed after embedding are re-embedded.
- **Later: hybrid**: full-text and vector results are merged with Reciprocal Rank
  Fusion.
- The active backend(s) are chosen by the setting `search.backends` (default
  `["sqlite-fts"]`).

## Consequences

- The search CLI and MCP tool have one stable output shape regardless of backend.
- The trigram index is about 6× the text size, which is fine up to tens of thousands
  of entries. If that becomes a problem, a Japanese tokenizer (e.g. lindera as a
  custom FTS5 tokenizer) can be added as another backend without touching callers.

## Alternatives considered

- **Design only for FTS now**: would force a later refactor of every caller.
- **Vector-only search**: poor for exact identifiers, names and numbers, which make
  up a large share of real queries ("what did we decide about CSV for client X").
