//! Entries, raw object rows, sections and summaries.

use chrono::{DateTime, Utc};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, Row, params, params_from_iter};
use sb_core::util::{sha256_parts, ts};
use sb_core::{
    AccountId, EntryOrigin, Generator, GeneratorKind, RawMode, RawRole, RawStatus, SectionDraft,
    SectionKind, SectionOrigin, SourceKind, SourceRef, SummaryStatus, Usage,
};
use serde::Serialize;
use serde_json::Value;

use crate::catalog::{Catalog, OptionalExt, opt_ts, parse_col, parse_json, req_ts};
use crate::error::{Result, StoreError};
use crate::fts;

/// An entry row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub id: i64,
    pub entry_uid: String,
    pub account_id: String,
    pub source_kind: SourceKind,
    pub source_id: String,
    pub source_url: Option<String>,
    pub title: String,
    pub source_created_at: Option<DateTime<Utc>>,
    pub source_updated_at: Option<DateTime<Utc>>,
    pub ingested_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub raw_status: RawStatus,
    pub raw_hash: Option<String>,
    pub summary_status: SummaryStatus,
    pub summary_attempts: i64,
    pub summary_error: Option<String>,
    pub fetch_state: Option<Value>,
    pub metadata: Value,
    pub origin: EntryOrigin,
}

impl Entry {
    /// The entry date: when the original was created, else when it was ingested.
    pub fn date(&self) -> DateTime<Utc> {
        self.source_created_at.unwrap_or(self.ingested_at)
    }

    /// The link to cite (cli.md): `source_url`, then `metadata.transcript_url`.
    /// The caller falls back to the local raw path.
    pub fn cite_url(&self) -> Option<String> {
        self.source_url.clone().or_else(|| {
            self.metadata
                .get("transcript_url")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
    }

    pub fn source_ref(&self) -> Result<SourceRef> {
        Ok(SourceRef {
            account_id: AccountId::new(self.account_id.clone())
                .map_err(|e| StoreError::Invalid(e.to_string()))?,
            source_kind: self.source_kind,
            source_id: self.source_id.clone(),
            source_url: self.source_url.clone(),
            created_at: self.source_created_at,
            updated_at: self.source_updated_at,
        })
    }

    pub(crate) fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        let fetch_state: Option<String> = r.get(16)?;
        Ok(Entry {
            id: r.get(0)?,
            entry_uid: r.get(1)?,
            account_id: r.get(2)?,
            source_kind: parse_col(r.get(3)?, 3)?,
            source_id: r.get(4)?,
            source_url: r.get(5)?,
            title: r.get(6)?,
            source_created_at: opt_ts(r.get(7)?),
            source_updated_at: opt_ts(r.get(8)?),
            ingested_at: req_ts(r.get(9)?),
            updated_at: req_ts(r.get(10)?),
            raw_status: parse_col(r.get(11)?, 11)?,
            raw_hash: r.get(12)?,
            summary_status: parse_col(r.get(13)?, 13)?,
            summary_attempts: r.get(14)?,
            summary_error: r.get(15)?,
            fetch_state: fetch_state.map(|s| parse_json(s, 16)).transpose()?,
            metadata: parse_json(r.get(17)?, 17)?,
            origin: parse_col(r.get(18)?, 18)?,
        })
    }
}

pub(crate) const ENTRY_COLS: &str = "e.id, e.entry_uid, e.account_id, e.source_kind, e.source_id, e.source_url, e.title, \
    e.source_created_at, e.source_updated_at, e.ingested_at, e.updated_at, e.raw_status, e.raw_hash, \
    e.summary_status, e.summary_attempts, e.summary_error, e.fetch_state, e.metadata, e.origin";

/// A section row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Section {
    pub id: i64,
    pub entry_id: i64,
    pub kind: SectionKind,
    pub origin: SectionOrigin,
    pub position: i64,
    pub text: String,
}

/// A summary provenance row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SummaryRecord {
    pub entry_id: i64,
    pub generator_kind: GeneratorKind,
    pub provider: String,
    pub model: String,
    pub profile: Option<String>,
    pub prompt_version: Option<String>,
    pub input_hash: String,
    pub generated_at: DateTime<Utc>,
    pub usage: Option<Value>,
}

impl SummaryRecord {
    pub fn generator(&self) -> Generator {
        Generator {
            kind: self.generator_kind,
            provider: self.provider.clone(),
            model: self.model.clone(),
            prompt_version: self.prompt_version.clone(),
        }
    }
}

/// A raw object row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RawObjectRow {
    pub id: i64,
    pub entry_id: i64,
    pub role: RawRole,
    pub seq: i64,
    /// Relative to the home, `/`-separated.
    pub path: String,
    pub media_type: String,
    pub sha256: String,
    pub size: i64,
    pub fetched_at: DateTime<Utc>,
}

/// A raw object written to disk and ready to be recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredRaw {
    pub role: RawRole,
    pub seq: i64,
    pub path: String,
    pub media_type: String,
    pub sha256: String,
    pub size: i64,
}

/// What to do with the summary state after normalization.
#[derive(Debug, Clone, PartialEq)]
pub enum SummaryDecision {
    /// Leave summary status and generated sections alone.
    Keep,
    /// The summary input changed: summarize again. Old generated sections stay
    /// searchable until they are replaced.
    Pending,
    /// Below the thresholds.
    Skipped,
    /// The source has no summary step.
    NoSummary,
    /// The normalized sections include a source-native summary.
    Native {
        generator: Generator,
        input_hash: String,
    },
}

/// Normalized content to write.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedUpdate {
    pub title: String,
    pub source_url: Option<String>,
    pub source_created_at: Option<DateTime<Utc>>,
    pub source_updated_at: Option<DateTime<Utc>>,
    pub sections: Vec<SectionDraft>,
    pub summary: SummaryDecision,
}

/// An upsert of one entry by natural key.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryUpdate {
    pub source_ref: SourceRef,
    pub origin: EntryOrigin,
    /// Raw objects written for this update.
    pub raw: Option<(RawMode, Vec<StoredRaw>)>,
    pub fetch_state: Option<Value>,
    /// Full metadata to store (the caller merges).
    pub metadata: Option<Value>,
    pub normalized: Option<NormalizedUpdate>,
}

/// Result of an upsert.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UpsertResult {
    pub entry_id: i64,
    pub created: bool,
    /// Raw paths no longer referenced (after a replace); delete after commit.
    pub stale_paths: Vec<String>,
}

/// Filters shared by list, refetch, reextract, resummarize and review.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EntryFilter {
    pub accounts: Vec<String>,
    pub source_kinds: Vec<SourceKind>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub entry_uids: Vec<String>,
    pub raw_status: Vec<RawStatus>,
    pub summary_status: Vec<SummaryStatus>,
    pub where_model: Option<String>,
    pub where_provider: Option<String>,
    /// Slack channel name or ID (`metadata.channel_name` / `channel_id`).
    pub channel: Option<String>,
    pub has_summary: Option<bool>,
    pub limit: Option<u64>,
    /// Only entries with `id > after_id` (keyset pagination, oldest first).
    pub after_id: Option<i64>,
    /// Order by ascending id instead of newest first.
    pub ascending_id: bool,
}

impl EntryFilter {
    /// Build a WHERE clause over `entries e` (and `summaries s` via subquery).
    pub(crate) fn where_sql(&self) -> (String, Vec<SqlValue>) {
        let mut conds: Vec<String> = Vec::new();
        let mut params: Vec<SqlValue> = Vec::new();
        fn in_list(
            conds: &mut Vec<String>,
            params: &mut Vec<SqlValue>,
            col: &str,
            vals: Vec<String>,
        ) {
            if vals.is_empty() {
                return;
            }
            let ph = vec!["?"; vals.len()].join(", ");
            conds.push(format!("{col} IN ({ph})"));
            params.extend(vals.into_iter().map(SqlValue::Text));
        }
        in_list(
            &mut conds,
            &mut params,
            "e.account_id",
            self.accounts.clone(),
        );
        in_list(
            &mut conds,
            &mut params,
            "e.source_kind",
            self.source_kinds.iter().map(|k| k.to_string()).collect(),
        );
        in_list(
            &mut conds,
            &mut params,
            "e.entry_uid",
            self.entry_uids.clone(),
        );
        in_list(
            &mut conds,
            &mut params,
            "e.raw_status",
            self.raw_status.iter().map(|k| k.to_string()).collect(),
        );
        in_list(
            &mut conds,
            &mut params,
            "e.summary_status",
            self.summary_status.iter().map(|k| k.to_string()).collect(),
        );
        if let Some(s) = self.since {
            conds.push("IFNULL(e.source_created_at, e.ingested_at) >= ?".into());
            params.push(SqlValue::Text(ts(s)));
        }
        if let Some(u) = self.until {
            conds.push("IFNULL(e.source_created_at, e.ingested_at) < ?".into());
            params.push(SqlValue::Text(ts(u)));
        }
        if let Some(m) = &self.where_model {
            conds.push(
                "EXISTS (SELECT 1 FROM summaries s WHERE s.entry_id = e.id AND s.model = ?)".into(),
            );
            params.push(SqlValue::Text(m.clone()));
        }
        if let Some(p) = &self.where_provider {
            conds.push(
                "EXISTS (SELECT 1 FROM summaries s WHERE s.entry_id = e.id AND s.provider = ?)"
                    .into(),
            );
            params.push(SqlValue::Text(p.clone()));
        }
        if let Some(c) = &self.channel {
            let name = c.trim_start_matches('#').to_string();
            conds.push(
                "(json_extract(e.metadata, '$.channel_name') = ? OR json_extract(e.metadata, '$.channel_id') = ?)"
                    .into(),
            );
            params.push(SqlValue::Text(name.clone()));
            params.push(SqlValue::Text(name));
        }
        match self.has_summary {
            Some(true) => {
                conds.push("EXISTS (SELECT 1 FROM summaries s WHERE s.entry_id = e.id)".into())
            }
            Some(false) => {
                conds.push("NOT EXISTS (SELECT 1 FROM summaries s WHERE s.entry_id = e.id)".into())
            }
            None => {}
        }
        if let Some(a) = self.after_id {
            conds.push("e.id > ?".into());
            params.push(SqlValue::Integer(a));
        }
        let sql = if conds.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conds.join(" AND "))
        };
        (sql, params)
    }
}

/// Precedence when two origins compete for the same section kind.
fn origin_rank(o: SectionOrigin) -> u8 {
    match o {
        SectionOrigin::Generated => 1,
        SectionOrigin::Extracted => 2,
        SectionOrigin::User => 3,
    }
}

/// Write sections of the given origin. Rows of that origin not in `drafts`
/// are deleted when `replace_origin` is set. A draft never overwrites a
/// section of a higher-precedence origin.
pub(crate) fn write_sections(
    conn: &Connection,
    entry_id: i64,
    origin: SectionOrigin,
    drafts: &[SectionDraft],
    replace_origin: bool,
) -> Result<()> {
    if replace_origin {
        conn.execute(
            "DELETE FROM sections WHERE entry_id = ?1 AND origin = ?2",
            params![entry_id, origin.as_str()],
        )?;
    }
    for d in drafts.iter().filter(|d| d.origin == origin) {
        if d.text.trim().is_empty() {
            continue;
        }
        let existing: Option<String> = conn
            .query_row(
                "SELECT origin FROM sections WHERE entry_id = ?1 AND kind = ?2",
                params![entry_id, d.kind.as_str()],
                |r| r.get(0),
            )
            .opt()?;
        if let Some(e) = existing {
            let e: SectionOrigin = e
                .parse()
                .map_err(|_| StoreError::Invalid(format!("section origin {e:?}")))?;
            if origin_rank(e) > origin_rank(origin) {
                continue;
            }
        }
        conn.execute(
            "INSERT INTO sections(entry_id, kind, origin, position, text) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(entry_id, kind) DO UPDATE SET origin = excluded.origin, position = excluded.position, text = excluded.text",
            params![entry_id, d.kind.as_str(), origin.as_str(), d.kind.position(), d.text],
        )?;
    }
    Ok(())
}

fn upsert_summary_row(
    conn: &Connection,
    entry_id: i64,
    generator: &Generator,
    profile: Option<&str>,
    input_hash: &str,
    generated_at: &str,
    usage: Option<&Value>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO summaries(entry_id, generator_kind, provider, model, profile, prompt_version, input_hash, generated_at, usage)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(entry_id) DO UPDATE SET generator_kind = excluded.generator_kind, provider = excluded.provider,
           model = excluded.model, profile = excluded.profile, prompt_version = excluded.prompt_version,
           input_hash = excluded.input_hash, generated_at = excluded.generated_at, usage = excluded.usage",
        params![
            entry_id,
            generator.kind.as_str(),
            generator.provider,
            generator.model,
            profile,
            // A source-native summary never gets a prompt version (invariant 4).
            if generator.kind == GeneratorKind::SourceNative { None } else { generator.prompt_version.clone() },
            input_hash,
            generated_at,
            usage.map(|u| u.to_string()),
        ],
    )?;
    Ok(())
}

/// Recompute `raw_hash` from the raw object rows, ordered by role then seq.
pub(crate) fn recompute_raw_hash(conn: &Connection, entry_id: i64) -> Result<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT sha256 FROM raw_objects WHERE entry_id = ?1 ORDER BY role, seq")?;
    let hashes: Vec<String> = stmt
        .query_map([entry_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if hashes.is_empty() {
        return Ok(None);
    }
    Ok(Some(sha256_parts(hashes.iter().map(|h| h.as_bytes()))))
}

/// Upsert an entry by natural key inside a transaction.
pub(crate) fn upsert_entry_tx(
    conn: &Connection,
    now: &str,
    u: &EntryUpdate,
) -> Result<UpsertResult> {
    let r = &u.source_ref;
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM entries WHERE account_id = ?1 AND source_kind = ?2 AND source_id = ?3",
            params![r.account_id.as_str(), r.source_kind.as_str(), r.source_id],
            |row| row.get(0),
        )
        .opt()?;
    let mut result = UpsertResult::default();
    let entry_id = match existing {
        Some(id) => id,
        None => {
            let title = u
                .normalized
                .as_ref()
                .map(|n| n.title.clone())
                .unwrap_or_else(|| r.source_id.clone());
            conn.execute(
                "INSERT INTO entries(entry_uid, account_id, source_kind, source_id, source_url, title,
                   source_created_at, source_updated_at, ingested_at, updated_at, raw_status, summary_status, metadata, origin)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, 'missing', 'none', '{}', ?10)",
                params![
                    ulid::Ulid::new().to_string(),
                    r.account_id.as_str(),
                    r.source_kind.as_str(),
                    r.source_id,
                    r.source_url,
                    title,
                    r.created_at.map(ts),
                    r.updated_at.map(ts),
                    now,
                    u.origin.as_str(),
                ],
            )?;
            result.created = true;
            conn.last_insert_rowid()
        }
    };
    result.entry_id = entry_id;

    if let Some((mode, objects)) = &u.raw {
        if *mode == RawMode::Replace {
            let mut stmt = conn.prepare("SELECT path FROM raw_objects WHERE entry_id = ?1")?;
            let old: Vec<String> = stmt
                .query_map([entry_id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let new_paths: Vec<&str> = objects.iter().map(|o| o.path.as_str()).collect();
            result.stale_paths = old
                .into_iter()
                .filter(|p| !new_paths.contains(&p.as_str()))
                .collect();
            conn.execute("DELETE FROM raw_objects WHERE entry_id = ?1", [entry_id])?;
        }
        for o in objects {
            conn.execute(
                "INSERT INTO raw_objects(entry_id, role, seq, path, media_type, sha256, size, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(entry_id, role, seq) DO UPDATE SET path = excluded.path, media_type = excluded.media_type,
                   sha256 = excluded.sha256, size = excluded.size, fetched_at = excluded.fetched_at",
                params![entry_id, o.role.as_str(), o.seq, o.path, o.media_type, o.sha256, o.size, now],
            )?;
        }
        let hash = recompute_raw_hash(conn, entry_id)?;
        conn.execute(
            "UPDATE entries SET raw_hash = ?2, raw_status = ?3 WHERE id = ?1",
            params![
                entry_id,
                hash,
                if hash.is_some() { "present" } else { "missing" }
            ],
        )?;
    }
    if let Some(fs) = &u.fetch_state {
        conn.execute(
            "UPDATE entries SET fetch_state = ?2 WHERE id = ?1",
            params![entry_id, fs.to_string()],
        )?;
    }
    if let Some(m) = &u.metadata {
        conn.execute(
            "UPDATE entries SET metadata = ?2 WHERE id = ?1",
            params![entry_id, m.to_string()],
        )?;
    }
    if let Some(n) = &u.normalized {
        fts::remove_entry(conn, entry_id)?;
        conn.execute(
            "UPDATE entries SET title = ?2, source_url = IFNULL(?3, source_url),
               source_created_at = IFNULL(?4, source_created_at), source_updated_at = IFNULL(?5, source_updated_at)
             WHERE id = ?1",
            params![
                entry_id,
                n.title,
                n.source_url,
                n.source_created_at.map(ts),
                n.source_updated_at.map(ts)
            ],
        )?;
        write_sections(conn, entry_id, SectionOrigin::Extracted, &n.sections, true)?;
        write_sections(conn, entry_id, SectionOrigin::User, &n.sections, false)?;
        match &n.summary {
            SummaryDecision::Keep => {}
            SummaryDecision::Pending => {
                set_summary_status(conn, entry_id, SummaryStatus::Pending, true)?
            }
            SummaryDecision::Skipped => {
                set_summary_status(conn, entry_id, SummaryStatus::Skipped, true)?
            }
            SummaryDecision::NoSummary => {
                set_summary_status(conn, entry_id, SummaryStatus::None, true)?
            }
            SummaryDecision::Native {
                generator,
                input_hash,
            } => {
                write_sections(conn, entry_id, SectionOrigin::Generated, &n.sections, true)?;
                upsert_summary_row(
                    conn,
                    entry_id,
                    generator,
                    Some("native"),
                    input_hash,
                    now,
                    None,
                )?;
                set_summary_status(conn, entry_id, SummaryStatus::Done, true)?;
            }
        }
        fts::index_entry(conn, entry_id)?;
    }
    conn.execute(
        "UPDATE entries SET updated_at = ?2 WHERE id = ?1",
        params![entry_id, now],
    )?;
    Ok(result)
}

fn set_summary_status(
    conn: &Connection,
    entry_id: i64,
    status: SummaryStatus,
    reset: bool,
) -> Result<()> {
    if reset {
        conn.execute(
            "UPDATE entries SET summary_status = ?2, summary_attempts = 0, summary_error = NULL WHERE id = ?1",
            params![entry_id, status.as_str()],
        )?;
    } else {
        conn.execute(
            "UPDATE entries SET summary_status = ?2 WHERE id = ?1",
            params![entry_id, status.as_str()],
        )?;
    }
    Ok(())
}

/// A finished summary to commit.
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryCommit {
    pub entry_id: i64,
    pub sections: Vec<SectionDraft>,
    pub generator: Generator,
    pub profile: Option<String>,
    pub input_hash: String,
    pub usage: Usage,
    /// Estimated cost of the attempt (provider-reported, or tokens times price);
    /// `None` when unknown. Recorded in the usage ledger (ADR-0013).
    pub cost_usd: Option<f64>,
    /// The run that made the call, when there is one.
    pub run_id: Option<i64>,
}

impl Catalog {
    /// Upsert one entry in its own transaction.
    pub fn upsert_entry(&self, u: &EntryUpdate) -> Result<UpsertResult> {
        let now = self.now_ts();
        self.with_tx(|tx| upsert_entry_tx(tx, &now, u))
    }

    pub fn entry(&self, id: i64) -> Result<Option<Entry>> {
        self.conn
            .query_row(
                &format!("SELECT {ENTRY_COLS} FROM entries e WHERE e.id = ?1"),
                [id],
                Entry::from_row,
            )
            .opt()
    }

    pub fn entry_by_uid(&self, uid: &str) -> Result<Option<Entry>> {
        self.conn
            .query_row(
                &format!("SELECT {ENTRY_COLS} FROM entries e WHERE e.entry_uid = ?1"),
                [uid],
                Entry::from_row,
            )
            .opt()
    }

    pub fn entry_by_key(
        &self,
        account: &str,
        kind: SourceKind,
        source_id: &str,
    ) -> Result<Option<Entry>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {ENTRY_COLS} FROM entries e WHERE e.account_id = ?1 AND e.source_kind = ?2 AND e.source_id = ?3"
                ),
                params![account, kind.as_str(), source_id],
                Entry::from_row,
            )
            .opt()
    }

    /// Entries of a source kind with this `source_id`, in any account (the same
    /// Drive file can be known to several accounts).
    pub fn entries_by_source_id(&self, kind: SourceKind, source_id: &str) -> Result<Vec<Entry>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ENTRY_COLS} FROM entries e WHERE e.source_kind = ?1 AND e.source_id = ?2 ORDER BY e.id"
        ))?;
        let rows = stmt
            .query_map(params![kind.as_str(), source_id], Entry::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Entries of a source kind whose metadata has a top-level text key with this
    /// value (for example `original_sha256`). `key` must be a plain identifier.
    pub fn entries_by_metadata_text(
        &self,
        kind: SourceKind,
        key: &str,
        value: &str,
    ) -> Result<Vec<Entry>> {
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ENTRY_COLS} FROM entries e WHERE e.source_kind = ?1 AND json_extract(e.metadata, '$.{key}') = ?2 ORDER BY e.id"
        ))?;
        let rows = stmt
            .query_map(params![kind.as_str(), value], Entry::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// List entries matching a filter, newest first (or by id when
    /// `ascending_id` is set).
    pub fn list_entries(&self, f: &EntryFilter) -> Result<Vec<Entry>> {
        let (w, mut p) = f.where_sql();
        let order = if f.ascending_id {
            "e.id ASC"
        } else {
            "IFNULL(e.source_created_at, e.ingested_at) DESC, e.id DESC"
        };
        let mut sql = format!("SELECT {ENTRY_COLS} FROM entries e {w} ORDER BY {order}");
        if let Some(l) = f.limit {
            sql.push_str(" LIMIT ?");
            p.push(SqlValue::Integer(l as i64));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(p), Entry::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn count_entries(&self, f: &EntryFilter) -> Result<u64> {
        let (w, p) = f.where_sql();
        let n: i64 = self.conn.query_row(
            &format!("SELECT count(*) FROM entries e {w}"),
            params_from_iter(p),
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    pub fn sections(&self, entry_id: i64) -> Result<Vec<Section>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, entry_id, kind, origin, position, text FROM sections WHERE entry_id = ?1 ORDER BY position, id",
        )?;
        let rows = stmt
            .query_map([entry_id], |r| {
                Ok(Section {
                    id: r.get(0)?,
                    entry_id: r.get(1)?,
                    kind: parse_col(r.get(2)?, 2)?,
                    origin: parse_col(r.get(3)?, 3)?,
                    position: r.get(4)?,
                    text: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn summary(&self, entry_id: i64) -> Result<Option<SummaryRecord>> {
        self.conn
            .query_row(
                "SELECT entry_id, generator_kind, provider, model, profile, prompt_version, input_hash, generated_at, usage
                 FROM summaries WHERE entry_id = ?1",
                [entry_id],
                |r| {
                    let usage: Option<String> = r.get(8)?;
                    Ok(SummaryRecord {
                        entry_id: r.get(0)?,
                        generator_kind: parse_col(r.get(1)?, 1)?,
                        provider: r.get(2)?,
                        model: r.get(3)?,
                        profile: r.get(4)?,
                        prompt_version: r.get(5)?,
                        input_hash: r.get(6)?,
                        generated_at: req_ts(r.get(7)?),
                        usage: usage.map(|u| parse_json(u, 8)).transpose()?,
                    })
                },
            )
            .opt()
    }

    pub fn raw_objects(&self, entry_id: i64) -> Result<Vec<RawObjectRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, entry_id, role, seq, path, media_type, sha256, size, fetched_at
             FROM raw_objects WHERE entry_id = ?1 ORDER BY role, seq",
        )?;
        let rows = stmt
            .query_map([entry_id], |r| {
                Ok(RawObjectRow {
                    id: r.get(0)?,
                    entry_id: r.get(1)?,
                    role: parse_col(r.get(2)?, 2)?,
                    seq: r.get(3)?,
                    path: r.get(4)?,
                    media_type: r.get(5)?,
                    sha256: r.get(6)?,
                    size: r.get(7)?,
                    fetched_at: req_ts(r.get(8)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// All raw paths referenced by the catalog.
    pub fn all_raw_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM raw_objects")?;
        let rows = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    /// Commit a summary: replace generated sections and the summary row, mark
    /// done and re-index, in one transaction (ADR-0005, ADR-0012).
    pub fn commit_summary(&self, c: &SummaryCommit) -> Result<()> {
        let now = self.now_ts();
        let usage = serde_json::to_value(&c.usage)?;
        self.with_tx(|tx| {
            fts::remove_entry(tx, c.entry_id)?;
            write_sections(tx, c.entry_id, SectionOrigin::Generated, &c.sections, true)?;
            upsert_summary_row(
                tx,
                c.entry_id,
                &c.generator,
                c.profile.as_deref(),
                &c.input_hash,
                &now,
                Some(&usage),
            )?;
            set_summary_status(tx, c.entry_id, SummaryStatus::Done, true)?;
            tx.execute(
                "UPDATE entries SET updated_at = ?2 WHERE id = ?1",
                params![c.entry_id, now],
            )?;
            fts::index_entry(tx, c.entry_id)?;
            // The ledger row is written with the summary it paid for (ADR-0013).
            crate::budget::insert_usage(
                tx,
                &now,
                &crate::budget::NewUsage {
                    run_id: c.run_id,
                    entry_id: Some(c.entry_id),
                    profile: c.profile.clone().unwrap_or_default(),
                    generator: c.generator.clone(),
                    usage: c.usage.clone(),
                    cost_usd: c.cost_usd,
                    outcome: crate::budget::UsageOutcome::Ok,
                },
            )?;
            Ok(())
        })
    }

    /// Record a failed summary attempt.
    pub fn record_summary_failure(&self, entry_id: i64, error: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE entries SET summary_status = 'failed', summary_attempts = summary_attempts + 1, summary_error = ?2
             WHERE id = ?1",
            params![entry_id, error],
        )?;
        Ok(())
    }

    /// Set the summary status without touching attempts.
    pub fn set_summary_status(&self, entry_id: i64, status: SummaryStatus) -> Result<()> {
        set_summary_status(&self.conn, entry_id, status, false)
    }

    /// Reset attempt counters (for `--retry-failed`).
    pub fn reset_summary_attempts(&self, f: &EntryFilter) -> Result<u64> {
        let (w, p) = f.where_sql();
        let n = self.conn.execute(
            &format!(
                "UPDATE entries SET summary_attempts = 0 WHERE id IN (SELECT e.id FROM entries e {w}) AND summary_status = 'failed'"
            ),
            params_from_iter(p),
        )?;
        Ok(n as u64)
    }

    /// Entries due for summarization: pending or failed, below the attempt limit.
    pub fn summarization_candidates(
        &self,
        f: &EntryFilter,
        max_attempts: i64,
    ) -> Result<Vec<Entry>> {
        let (w, mut p) = f.where_sql();
        let extra = "e.summary_status IN ('pending', 'failed') AND e.summary_attempts < ?";
        let w = if w.is_empty() {
            format!("WHERE {extra}")
        } else {
            format!("{w} AND {extra}")
        };
        p.push(SqlValue::Integer(max_attempts));
        let mut sql = format!(
            "SELECT {ENTRY_COLS} FROM entries e {w} ORDER BY IFNULL(e.source_created_at, e.ingested_at) DESC, e.id DESC"
        );
        if let Some(l) = f.limit {
            sql.push_str(" LIMIT ?");
            p.push(SqlValue::Integer(l as i64));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(p), Entry::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Mark an entry's raw status (e.g. `missing` after a consistency check,
    /// `fetch_failed` after a refetch error).
    pub fn set_raw_status(&self, entry_id: i64, status: RawStatus) -> Result<()> {
        self.conn.execute(
            "UPDATE entries SET raw_status = ?2, updated_at = ?3 WHERE id = ?1",
            params![entry_id, status.as_str(), self.now_ts()],
        )?;
        Ok(())
    }

    /// Update the metadata of an entry.
    pub fn set_entry_metadata(&self, entry_id: i64, metadata: &Value) -> Result<()> {
        self.conn.execute(
            "UPDATE entries SET metadata = ?2 WHERE id = ?1",
            params![entry_id, metadata.to_string()],
        )?;
        Ok(())
    }

    /// Replace a user section (e.g. `background`).
    pub fn set_user_section(&self, entry_id: i64, kind: SectionKind, text: &str) -> Result<()> {
        self.with_tx(|tx| {
            fts::remove_entry(tx, entry_id)?;
            let draft = SectionDraft {
                kind,
                origin: SectionOrigin::User,
                text: text.to_string(),
            };
            write_sections(tx, entry_id, SectionOrigin::User, &[draft], false)?;
            fts::index_entry(tx, entry_id)?;
            Ok(())
        })
    }

    /// Delete an entry and its index rows; returns its raw paths for deletion.
    pub fn delete_entry(&self, entry_id: i64) -> Result<Vec<String>> {
        let paths = self
            .raw_objects(entry_id)?
            .into_iter()
            .map(|r| r.path)
            .collect();
        self.with_tx(|tx| {
            fts::remove_entry(tx, entry_id)?;
            tx.execute("DELETE FROM entries WHERE id = ?1", [entry_id])?;
            Ok(())
        })?;
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::test_util::temp_catalog;
    use sb_core::AccountKind;

    pub(crate) fn sref(id: &str) -> SourceRef {
        SourceRef {
            account_id: AccountId::new("acme").unwrap(),
            source_kind: SourceKind::SlackThread,
            source_id: id.into(),
            source_url: Some(format!("https://slack.example/{id}")),
            created_at: sb_core::util::parse_ts("2026-09-01T00:00:00Z"),
            updated_at: None,
        }
    }

    fn raw(seq: i64, sha: &str) -> StoredRaw {
        StoredRaw {
            role: RawRole::Primary,
            seq,
            path: format!("raw/x/primary.{seq}.jsonl"),
            media_type: "application/x-ndjson".into(),
            sha256: sha.into(),
            size: 1,
        }
    }

    fn normalized(text: &str, summary: SummaryDecision) -> NormalizedUpdate {
        NormalizedUpdate {
            title: "#general hello".into(),
            source_url: None,
            source_created_at: None,
            source_updated_at: None,
            sections: vec![SectionDraft {
                kind: SectionKind::Details,
                origin: SectionOrigin::Extracted,
                text: text.into(),
            }],
            summary,
        }
    }

    #[test]
    fn upsert_append_replace_and_summary() {
        let (_d, cat) = temp_catalog();
        cat.ensure_account(&AccountId::new("acme").unwrap(), AccountKind::Slack, "Acme")
            .unwrap();
        let mut u = EntryUpdate {
            source_ref: sref("C1:1.0"),
            origin: EntryOrigin::Sync,
            raw: Some((RawMode::Replace, vec![raw(0, "aa")])),
            fetch_state: Some(serde_json::json!({"last_ts": "1.0"})),
            metadata: Some(serde_json::json!({"channel_name": "general"})),
            normalized: Some(normalized("09:00 Alice: hello", SummaryDecision::Pending)),
        };
        let r1 = cat.upsert_entry(&u).unwrap();
        assert!(r1.created);
        let e = cat.entry(r1.entry_id).unwrap().unwrap();
        assert_eq!(e.raw_status, RawStatus::Present);
        assert_eq!(e.summary_status, SummaryStatus::Pending);
        let h1 = e.raw_hash.clone().unwrap();

        // Append a segment.
        u.raw = Some((RawMode::Append, vec![raw(1, "bb")]));
        let r2 = cat.upsert_entry(&u).unwrap();
        assert!(!r2.created);
        assert_eq!(r2.entry_id, r1.entry_id);
        assert_eq!(cat.raw_objects(r1.entry_id).unwrap().len(), 2);
        assert_ne!(
            cat.entry(r1.entry_id).unwrap().unwrap().raw_hash.unwrap(),
            h1
        );

        // Summary commit.
        cat.commit_summary(&SummaryCommit {
            entry_id: r1.entry_id,
            sections: vec![SectionDraft {
                kind: SectionKind::Overview,
                origin: SectionOrigin::Generated,
                text: "They said hello.".into(),
            }],
            generator: Generator {
                kind: GeneratorKind::LlmApi,
                provider: "anthropic".into(),
                model: "m".into(),
                prompt_version: Some("conversation-summary/v1".into()),
            },
            profile: Some("fast".into()),
            input_hash: "h".into(),
            usage: Usage {
                input_tokens: 1000,
                output_tokens: 100,
                calls: 2,
                ..Default::default()
            },
            cost_usd: Some(0.5),
            run_id: Some(7),
        })
        .unwrap();
        // The ledger row was written with the summary (ADR-0013).
        let (spend, since) = cat.spend_total().unwrap();
        assert_eq!((spend.cost_usd, spend.calls), (0.5, 2));
        assert!(since.is_some());
        assert_eq!(
            cat.entry(r1.entry_id).unwrap().unwrap().summary_status,
            SummaryStatus::Done
        );
        assert_eq!(cat.sections(r1.entry_id).unwrap().len(), 2);

        // Replace drops segment 1.
        u.raw = Some((RawMode::Replace, vec![raw(0, "cc")]));
        u.normalized = Some(normalized(
            "09:00 Alice: hello again",
            SummaryDecision::Keep,
        ));
        let r3 = cat.upsert_entry(&u).unwrap();
        assert_eq!(r3.stale_paths, vec!["raw/x/primary.1.jsonl".to_string()]);
        let secs = cat.sections(r1.entry_id).unwrap();
        assert_eq!(secs.len(), 2, "generated overview kept, details replaced");
        assert!(secs.iter().any(|s| s.text.contains("again")));
    }

    #[test]
    fn filters() {
        let (_d, cat) = temp_catalog();
        cat.ensure_account(&AccountId::new("acme").unwrap(), AccountKind::Slack, "Acme")
            .unwrap();
        for i in 0..3 {
            cat.upsert_entry(&EntryUpdate {
                source_ref: sref(&format!("C1:{i}.0")),
                origin: EntryOrigin::Sync,
                raw: None,
                fetch_state: None,
                metadata: Some(serde_json::json!({"channel_name": "general", "channel_id": "C1"})),
                normalized: Some(normalized("x", SummaryDecision::Pending)),
            })
            .unwrap();
        }
        let f = EntryFilter {
            channel: Some("#general".into()),
            limit: Some(2),
            ..Default::default()
        };
        assert_eq!(cat.list_entries(&f).unwrap().len(), 2);
        let f = EntryFilter {
            raw_status: vec![RawStatus::Missing],
            ..Default::default()
        };
        assert_eq!(cat.count_entries(&f).unwrap(), 3);
        assert_eq!(
            cat.summarization_candidates(&EntryFilter::default(), 3)
                .unwrap()
                .len(),
            3
        );
    }
}
