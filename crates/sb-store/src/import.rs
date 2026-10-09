//! Catalog writes for `sb import` (import-format.md).

use chrono::{DateTime, Utc};
use rusqlite::params;
use second_brain_kernel::util::ts;
use second_brain_kernel::{Generator, SectionDraft, SectionOrigin, SourceRef};
use serde_json::Value;

use crate::catalog::{Catalog, OptionalExt};
use crate::entries::{StoredRaw, recompute_raw_hash, write_sections};
use crate::error::Result;
use crate::fts;

/// An imported entry to write (new, or existing without raw data).
#[derive(Debug, Clone, PartialEq)]
pub struct ImportWrite {
    pub source_ref: SourceRef,
    pub title: String,
    pub ingested_at: Option<DateTime<Utc>>,
    pub metadata: Value,
    pub sections: Vec<SectionDraft>,
    pub summary: Option<(Generator, Option<DateTime<Utc>>)>,
    pub raw: Vec<StoredRaw>,
}

/// What the import did with a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportResult {
    Created,
    Updated,
    Unchanged,
}

impl Catalog {
    /// Write an imported entry. Sections of the entry are replaced by the
    /// imported ones; the caller never calls this for entries whose raw data
    /// is present (never downgrade).
    pub fn apply_import(&self, w: &ImportWrite) -> Result<(ImportResult, i64)> {
        let now = self.now_ts();
        self.with_tx(|tx| {
            let r = &w.source_ref;
            let existing: Option<(i64, String)> = tx
                .query_row(
                    "SELECT id, ingested_at FROM entries WHERE account_id = ?1 AND source_kind = ?2 AND source_id = ?3",
                    params![r.account_id.as_str(), r.source_kind.as_str(), r.source_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .opt()?;
            let ingested = w.ingested_at.map(ts).unwrap_or_else(|| now.clone());
            let has_generated = w.sections.iter().any(|s| s.origin == SectionOrigin::Generated);
            let summary_status = if w.summary.is_some() || has_generated { "done" } else { "none" };
            let (entry_id, result) = match existing {
                Some((id, old_ingested)) => {
                    if self.import_is_unchanged(tx, id, w)? {
                        return Ok((ImportResult::Unchanged, id));
                    }
                    let ingested = if old_ingested < ingested { old_ingested } else { ingested };
                    tx.execute(
                        "UPDATE entries SET source_url = IFNULL(?2, source_url), title = ?3, source_created_at = IFNULL(?4, source_created_at),
                           source_updated_at = IFNULL(?5, source_updated_at), ingested_at = ?6, updated_at = ?7, metadata = ?8,
                           summary_status = ?9 WHERE id = ?1",
                        params![
                            id,
                            r.source_url,
                            w.title,
                            r.created_at.map(ts),
                            r.updated_at.map(ts),
                            ingested,
                            now,
                            w.metadata.to_string(),
                            summary_status
                        ],
                    )?;
                    (id, ImportResult::Updated)
                }
                None => {
                    tx.execute(
                        "INSERT INTO entries(entry_uid, account_id, source_kind, source_id, source_url, title,
                           source_created_at, source_updated_at, ingested_at, updated_at, raw_status, summary_status, metadata, origin)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'missing', ?11, ?12, 'import')",
                        params![
                            ulid::Ulid::new().to_string(),
                            r.account_id.as_str(),
                            r.source_kind.as_str(),
                            r.source_id,
                            r.source_url,
                            w.title,
                            r.created_at.map(ts),
                            r.updated_at.map(ts),
                            ingested,
                            now,
                            summary_status,
                            w.metadata.to_string()
                        ],
                    )?;
                    (tx.last_insert_rowid(), ImportResult::Created)
                }
            };
            fts::remove_entry(tx, entry_id)?;
            tx.execute("DELETE FROM sections WHERE entry_id = ?1", [entry_id])?;
            for origin in [SectionOrigin::User, SectionOrigin::Extracted, SectionOrigin::Generated] {
                write_sections(tx, entry_id, origin, &w.sections, false)?;
            }
            if !w.raw.is_empty() {
                tx.execute("DELETE FROM raw_objects WHERE entry_id = ?1", [entry_id])?;
                for o in &w.raw {
                    tx.execute(
                        "INSERT INTO raw_objects(entry_id, role, seq, path, media_type, sha256, size, fetched_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![entry_id, o.role.as_str(), o.seq, o.path, o.media_type, o.sha256, o.size, now],
                    )?;
                }
                let hash = recompute_raw_hash(tx, entry_id)?;
                tx.execute(
                    "UPDATE entries SET raw_hash = ?2, raw_status = 'present' WHERE id = ?1",
                    params![entry_id, hash],
                )?;
            }
            tx.execute("DELETE FROM summaries WHERE entry_id = ?1", [entry_id])?;
            if summary_status == "done" {
                let g = w.summary.as_ref().map(|(g, _)| g.clone()).unwrap_or(Generator {
                    kind: second_brain_kernel::GeneratorKind::Unknown,
                    provider: "unknown".into(),
                    model: "unknown".into(),
                    prompt_version: None,
                });
                let generated_at = w
                    .summary
                    .as_ref()
                    .and_then(|(_, t)| *t)
                    .or(w.ingested_at)
                    .map(ts)
                    .unwrap_or_else(|| now.clone());
                tx.execute(
                    "INSERT INTO summaries(entry_id, generator_kind, provider, model, profile, prompt_version, input_hash, generated_at)
                     VALUES (?1, ?2, ?3, ?4, NULL, ?5, 'import', ?6)",
                    params![
                        entry_id,
                        g.kind.as_str(),
                        g.provider,
                        g.model,
                        if g.kind == second_brain_kernel::GeneratorKind::SourceNative { None } else { g.prompt_version },
                        generated_at
                    ],
                )?;
            }
            fts::index_entry(tx, entry_id)?;
            Ok((result, entry_id))
        })
    }

    fn import_is_unchanged(
        &self,
        conn: &rusqlite::Connection,
        id: i64,
        w: &ImportWrite,
    ) -> Result<bool> {
        let (title, metadata): (String, String) = conn.query_row(
            "SELECT title, metadata FROM entries WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if title != w.title
            || serde_json::from_str::<Value>(&metadata)? != w.metadata
            || !w.raw.is_empty()
        {
            return Ok(false);
        }
        let mut stmt = conn
            .prepare("SELECT kind, origin, text FROM sections WHERE entry_id = ?1 ORDER BY kind")?;
        let mut have: Vec<(String, String, String)> = stmt
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut want: Vec<(String, String, String)> = w
            .sections
            .iter()
            .map(|s| (s.kind.to_string(), s.origin.to_string(), s.text.clone()))
            .collect();
        have.sort();
        want.sort();
        Ok(have == want)
    }

    /// For an entry that already has raw data: only fill in what it lacks
    /// (an older `ingested_at`, the `import_ref`).
    pub fn merge_import_into_present(
        &self,
        entry_id: i64,
        ingested_at: Option<DateTime<Utc>>,
        import_ref: Option<&Value>,
    ) -> Result<bool> {
        let (old_ingested, metadata): (String, String) = self.conn.query_row(
            "SELECT ingested_at, metadata FROM entries WHERE id = ?1",
            [entry_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut changed = false;
        if let Some(t) = ingested_at.map(ts)
            && t < old_ingested
        {
            self.conn.execute(
                "UPDATE entries SET ingested_at = ?2 WHERE id = ?1",
                params![entry_id, t],
            )?;
            changed = true;
        }
        if let Some(r) = import_ref {
            let mut m: Value = serde_json::from_str(&metadata)?;
            if let Some(obj) = m.as_object_mut()
                && obj.get("import_ref") != Some(r)
            {
                obj.insert("import_ref".into(), r.clone());
                self.conn.execute(
                    "UPDATE entries SET metadata = ?2 WHERE id = ?1",
                    params![entry_id, m.to_string()],
                )?;
                changed = true;
            }
        }
        Ok(changed)
    }
}
