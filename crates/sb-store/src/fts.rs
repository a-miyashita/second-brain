//! The `sqlite-fts` search backend (ADR-0004, search.md): FTS5 trigram, plus a
//! `LIKE` path for terms shorter than three characters.

use std::collections::HashSet;

use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, params, params_from_iter};
use sb_core::search::{Capabilities, Hit, SearchBackend, SearchError, SearchMode, SearchQuery};
use sb_core::util::{nfkc, ts};

use crate::catalog::{Catalog, parse_col};
use crate::error::Result;

/// Backend name used in `search.backends`.
pub const NAME: &str = "sqlite-fts";

/// Snippet window (characters) for LIKE hits.
const SNIPPET_CHARS: usize = 80;

/// Remove all index rows of an entry. Call before changing its sections.
pub(crate) fn remove_entry(conn: &Connection, entry_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM fts_sections WHERE rowid IN (SELECT id FROM sections WHERE entry_id = ?1)",
        [entry_id],
    )?;
    Ok(())
}

/// Index all sections of an entry. Call after changing its sections.
pub(crate) fn index_entry(conn: &Connection, entry_id: i64) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO fts_sections(rowid, title, text)
         SELECT s.id, e.title, s.text FROM sections s JOIN entries e ON e.id = s.entry_id WHERE s.entry_id = ?1",
        [entry_id],
    )?;
    Ok(())
}

/// Rebuild the whole index.
pub(crate) fn rebuild(conn: &Connection) -> Result<u64> {
    conn.execute("DELETE FROM fts_sections", [])?;
    let n = conn.execute(
        "INSERT INTO fts_sections(rowid, title, text)
         SELECT s.id, e.title, s.text FROM sections s JOIN entries e ON e.id = s.entry_id",
        [],
    )?;
    conn.execute(
        "INSERT INTO fts_sections(fts_sections) VALUES ('optimize')",
        [],
    )?;
    Ok(n as u64)
}

/// Quote a term as an FTS5 phrase.
fn fts_phrase(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
}

/// Escape a term for `LIKE ... ESCAPE '\'`.
fn like_pattern(term: &str) -> String {
    let mut s = String::from("%");
    for c in term.chars() {
        if matches!(c, '%' | '_' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('%');
    s
}

/// A window of about `SNIPPET_CHARS` characters around the first match.
fn manual_snippet(text: &str, terms: &[String]) -> String {
    let lower = text.to_lowercase();
    let chars: Vec<char> = text.chars().collect();
    let lower_chars: Vec<char> = lower.chars().collect();
    let pos = terms
        .iter()
        .filter_map(|t| {
            let t: Vec<char> = t.to_lowercase().chars().collect();
            if t.is_empty() || lower_chars.len() != chars.len() {
                return None;
            }
            lower_chars.windows(t.len()).position(|w| w == t.as_slice())
        })
        .min()
        .unwrap_or(0);
    let start = pos.saturating_sub(SNIPPET_CHARS / 3);
    let end = (start + SNIPPET_CHARS).min(chars.len());
    let mut s: String = chars[start..end].iter().collect();
    s = s.replace('\n', " ");
    if start > 0 {
        s.insert(0, '…');
    }
    if end < chars.len() {
        s.push('…');
    }
    s
}

/// Filters on `sections s` / `entries e` shared by both paths.
fn filter_sql(q: &SearchQuery) -> (Vec<String>, Vec<SqlValue>) {
    let mut conds = Vec::new();
    let mut p = Vec::new();
    let in_list = |col: &str, vals: Vec<String>, conds: &mut Vec<String>, p: &mut Vec<SqlValue>| {
        if !vals.is_empty() {
            conds.push(format!("{col} IN ({})", vec!["?"; vals.len()].join(", ")));
            p.extend(vals.into_iter().map(SqlValue::Text));
        }
    };
    in_list(
        "s.kind",
        q.sections.iter().map(|k| k.to_string()).collect(),
        &mut conds,
        &mut p,
    );
    in_list(
        "e.source_kind",
        q.source_kinds.iter().map(|k| k.to_string()).collect(),
        &mut conds,
        &mut p,
    );
    in_list(
        "e.account_id",
        q.accounts.iter().map(|k| k.to_string()).collect(),
        &mut conds,
        &mut p,
    );
    if let Some(s) = q.since {
        conds.push("IFNULL(e.source_created_at, e.ingested_at) >= ?".into());
        p.push(SqlValue::Text(ts(s)));
    }
    if let Some(u) = q.until {
        conds.push("IFNULL(e.source_created_at, e.ingested_at) < ?".into());
        p.push(SqlValue::Text(ts(u)));
    }
    (conds, p)
}

/// Run a search against the catalog connection.
pub fn search(conn: &Connection, q: &SearchQuery) -> std::result::Result<Vec<Hit>, SearchError> {
    if matches!(q.mode, SearchMode::Vector | SearchMode::Hybrid) {
        return Err(SearchError::UnsupportedMode(q.mode));
    }
    let terms: Vec<String> = q
        .terms
        .iter()
        .map(|t| nfkc(t).trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() {
        return Err(SearchError::InvalidQuery("no search terms".into()));
    }
    let (long, short): (Vec<String>, Vec<String>) =
        terms.iter().cloned().partition(|t| t.chars().count() >= 3);
    let limit = q.effective_limit() as usize;
    let (mut conds, mut p) = filter_sql(q);
    for t in &short {
        conds.push("(s.text LIKE ? ESCAPE '\\' OR e.title LIKE ? ESCAPE '\\')".into());
        p.push(SqlValue::Text(like_pattern(t)));
        p.push(SqlValue::Text(like_pattern(t)));
    }
    // Fetch extra rows so that per-entry de-duplication still fills the limit.
    let fetch = if q.all_sections {
        limit
    } else {
        (limit * 20).max(200)
    };
    let be = |e: rusqlite::Error| SearchError::Backend(e.to_string());

    let mut rows: Vec<(i64, i64, String, f64, String)> = Vec::new(); // entry_id, section_id, kind, score, snippet
    if !long.is_empty() {
        let match_expr = long
            .iter()
            .map(|t| fts_phrase(t))
            .collect::<Vec<_>>()
            .join(" AND ");
        let extra = if conds.is_empty() {
            String::new()
        } else {
            format!("AND {}", conds.join(" AND "))
        };
        let sql = format!(
            "SELECT s.entry_id, s.id, s.kind, bm25(fts_sections, 2.0, 1.0) AS rank,
                    snippet(fts_sections, 1, '', '', '…', 40)
             FROM fts_sections JOIN sections s ON s.id = fts_sections.rowid JOIN entries e ON e.id = s.entry_id
             WHERE fts_sections MATCH ? {extra}
             ORDER BY rank LIMIT ?"
        );
        let mut params = vec![SqlValue::Text(match_expr)];
        params.extend(p);
        params.push(SqlValue::Integer(fetch as i64));
        let mut stmt = conn.prepare(&sql).map_err(be)?;
        let it = stmt
            .query_map(params_from_iter(params), |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    -r.get::<_, f64>(3)?,
                    r.get(4)?,
                ))
            })
            .map_err(be)?;
        for r in it {
            rows.push(r.map_err(be)?);
        }
    } else {
        let where_sql = if conds.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conds.join(" AND "))
        };
        let sql = format!(
            "SELECT s.entry_id, s.id, s.kind, s.text
             FROM sections s JOIN entries e ON e.id = s.entry_id {where_sql}
             ORDER BY IFNULL(e.source_created_at, e.ingested_at) DESC, e.id DESC, s.position
             LIMIT ?"
        );
        p.push(SqlValue::Integer(fetch as i64));
        let mut stmt = conn.prepare(&sql).map_err(be)?;
        let it = stmt
            .query_map(params_from_iter(p), |r| {
                let text: String = r.get(3)?;
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    0.0,
                    manual_snippet(&text, &short),
                ))
            })
            .map_err(be)?;
        for r in it {
            rows.push(r.map_err(be)?);
        }
    }

    let mut seen = HashSet::new();
    let mut hits = Vec::new();
    for (entry_id, _sid, kind, score, snippet) in rows {
        if !q.all_sections && !seen.insert(entry_id) {
            continue;
        }
        let section = parse_col(kind, 2).map_err(be)?;
        hits.push(Hit {
            entry_id,
            section,
            score: (score * 1000.0).round() / 1000.0,
            snippet: snippet.replace('\n', " "),
        });
        if hits.len() >= limit {
            break;
        }
    }
    Ok(hits)
}

/// The backend bound to a catalog.
pub struct SqliteFts<'a> {
    catalog: &'a Catalog,
}

impl<'a> SqliteFts<'a> {
    pub fn new(catalog: &'a Catalog) -> Self {
        SqliteFts { catalog }
    }
}

fn store_err(e: crate::error::StoreError) -> SearchError {
    SearchError::Backend(e.to_string())
}

impl SearchBackend for SqliteFts<'_> {
    fn name(&self) -> &'static str {
        NAME
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            full_text: true,
            vector: false,
            hybrid: false,
        }
    }

    fn index(&self, entry_id: i64) -> std::result::Result<(), SearchError> {
        self.catalog
            .with_tx(|tx| {
                remove_entry(tx, entry_id)?;
                index_entry(tx, entry_id)
            })
            .map_err(store_err)
    }

    fn remove(&self, entry_id: i64) -> std::result::Result<(), SearchError> {
        remove_entry(self.catalog.conn(), entry_id).map_err(store_err)
    }

    fn rebuild(&self) -> std::result::Result<u64, SearchError> {
        self.catalog.with_tx(|tx| rebuild(tx)).map_err(store_err)
    }

    fn search(&self, query: &SearchQuery) -> std::result::Result<Vec<Hit>, SearchError> {
        search(self.catalog.conn(), query)
    }
}

impl Catalog {
    /// Number of rows in the FTS index and in `sections` (doctor `index.consistency`).
    pub fn index_counts(&self) -> Result<(u64, u64)> {
        let fts: i64 = self
            .conn
            .query_row("SELECT count(*) FROM fts_sections", [], |r| r.get(0))?;
        let secs: i64 = self
            .conn
            .query_row("SELECT count(*) FROM sections", [], |r| r.get(0))?;
        Ok((fts as u64, secs as u64))
    }

    /// Re-index one entry (e.g. after a title change).
    pub fn reindex_entry(&self, entry_id: i64) -> Result<()> {
        self.with_tx(|tx| {
            remove_entry(tx, entry_id)?;
            index_entry(tx, entry_id)
        })
    }
}

/// Used by tests and `stats`: whether an FTS row exists for a section.
pub fn has_row(conn: &Connection, section_id: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT count(*) > 0 FROM fts_sections WHERE rowid = ?1",
        params![section_id],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::test_util::temp_catalog;
    use crate::entries::{EntryUpdate, NormalizedUpdate, SummaryDecision};
    use sb_core::{
        AccountId, AccountKind, EntryOrigin, SectionDraft, SectionKind, SectionOrigin, SourceKind,
        SourceRef,
    };

    fn add(cat: &Catalog, id: &str, title: &str, date: &str, secs: &[(SectionKind, &str)]) -> i64 {
        cat.upsert_entry(&EntryUpdate {
            source_ref: SourceRef {
                account_id: AccountId::new("work").unwrap(),
                source_kind: SourceKind::GoogleMeet,
                source_id: id.into(),
                source_url: None,
                created_at: sb_core::util::parse_ts(date),
                updated_at: None,
            },
            origin: EntryOrigin::Import,
            raw: None,
            fetch_state: None,
            metadata: None,
            normalized: Some(NormalizedUpdate {
                title: title.into(),
                source_url: None,
                source_created_at: None,
                source_updated_at: None,
                sections: secs
                    .iter()
                    .map(|(k, t)| SectionDraft {
                        kind: *k,
                        origin: SectionOrigin::User,
                        text: t.to_string(),
                    })
                    .collect(),
                summary: SummaryDecision::NoSummary,
            }),
        })
        .unwrap()
        .entry_id
    }

    fn setup() -> (tempfile::TempDir, Catalog) {
        let (d, cat) = temp_catalog();
        cat.ensure_account(
            &AccountId::new("work").unwrap(),
            AccountKind::Google,
            "Work",
        )
        .unwrap();
        add(
            &cat,
            "a",
            "Daily Standup",
            "2026-09-01T00:00:00Z",
            &[
                (
                    SectionKind::Overview,
                    "定例の打ち合わせ。契約の件を議論した。",
                ),
                (SectionKind::Decisions, "- CSV形式で進めることに決定した"),
            ],
        );
        add(
            &cat,
            "b",
            "契約レビュー",
            "2026-09-02T00:00:00Z",
            &[(SectionKind::Details, "契約書の要件を確認した。納期は来月。")],
        );
        add(
            &cat,
            "c",
            "Unrelated",
            "2026-09-03T00:00:00Z",
            &[(SectionKind::Overview, "Lunch plans.")],
        );
        (d, cat)
    }

    fn q(terms: &[&str]) -> SearchQuery {
        SearchQuery {
            terms: terms.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn long_terms_use_fts() {
        let (_d, cat) = setup();
        let hits = search(cat.conn(), &q(&["csv"])).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].section, SectionKind::Decisions);
        assert!(hits[0].snippet.contains("CSV"));
        assert!(hits[0].score > 0.0);
        // Full-width query is NFKC-normalized.
        assert_eq!(search(cat.conn(), &q(&["ＣＳＶ"])).unwrap().len(), 1);
    }

    #[test]
    fn two_char_japanese_terms_use_like() {
        let (_d, cat) = setup();
        let hits = search(cat.conn(), &q(&["契約"])).unwrap();
        assert_eq!(hits.len(), 2);
        // Newest first with score 0.
        assert_eq!(hits[0].score, 0.0);
        assert!(hits[0].snippet.contains("契約"));
        let first = cat.entry(hits[0].entry_id).unwrap().unwrap();
        assert_eq!(first.source_id, "b");
    }

    #[test]
    fn mixed_terms_intersect() {
        let (_d, cat) = setup();
        let hits = search(cat.conn(), &q(&["契約", "打ち合わせ"])).unwrap();
        assert_eq!(hits.len(), 1);
        let hits = search(cat.conn(), &q(&["納期", "打ち合わせ"])).unwrap();
        assert_eq!(hits.len(), 0);
    }

    #[test]
    fn section_filter_and_title_match() {
        let (_d, cat) = setup();
        let mut query = q(&["standup"]);
        let hits = search(cat.conn(), &query).unwrap();
        assert_eq!(hits.len(), 1, "title matches score");
        query.sections = vec![SectionKind::Decisions];
        let hits = search(cat.conn(), &query).unwrap();
        assert_eq!(hits[0].section, SectionKind::Decisions);
        query.all_sections = true;
        query.sections.clear();
        assert_eq!(search(cat.conn(), &query).unwrap().len(), 2);
    }

    #[test]
    fn rebuild_matches_sections() {
        let (_d, cat) = setup();
        let backend = SqliteFts::new(&cat);
        assert_eq!(backend.rebuild().unwrap(), 4);
        assert_eq!(cat.index_counts().unwrap(), (4, 4));
        assert_eq!(search(cat.conn(), &q(&["lunch"])).unwrap().len(), 1);
    }

    #[test]
    fn like_escapes_wildcards() {
        assert_eq!(like_pattern("a%_"), "%a\\%\\_%");
        assert_eq!(fts_phrase("say \"hi\""), "\"say \"\"hi\"\"\"");
    }
}
