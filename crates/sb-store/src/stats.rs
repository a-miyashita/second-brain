//! Aggregate counts for `sb stats`.

use serde::Serialize;

use crate::catalog::Catalog;
use crate::error::Result;

/// One counted group.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Count {
    pub key: String,
    pub count: u64,
}

/// Counts per account and source kind, with the date range.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceCount {
    pub account_id: String,
    pub source_kind: String,
    pub count: u64,
    pub oldest: Option<String>,
    pub newest: Option<String>,
}

/// Catalog statistics.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stats {
    pub entries: u64,
    pub sections: u64,
    pub by_source: Vec<SourceCount>,
    pub by_raw_status: Vec<Count>,
    pub by_summary_status: Vec<Count>,
    /// `generator_kind/provider/model`.
    pub by_summary_model: Vec<Count>,
    pub queue: u64,
}

impl Catalog {
    fn counts(&self, sql: &str) -> Result<Vec<Count>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Count {
                    key: r.get(0)?,
                    count: r.get::<_, i64>(1)? as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn stats(&self) -> Result<Stats> {
        let one = |sql: &str| -> Result<u64> {
            Ok(self.conn.query_row(sql, [], |r| r.get::<_, i64>(0))? as u64)
        };
        let mut stmt = self.conn.prepare(
            "SELECT account_id, source_kind, count(*), MIN(IFNULL(source_created_at, ingested_at)),
                    MAX(IFNULL(source_created_at, ingested_at))
             FROM entries GROUP BY account_id, source_kind ORDER BY account_id, source_kind",
        )?;
        let by_source = stmt
            .query_map([], |r| {
                Ok(SourceCount {
                    account_id: r.get(0)?,
                    source_kind: r.get(1)?,
                    count: r.get::<_, i64>(2)? as u64,
                    oldest: r.get(3)?,
                    newest: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Stats {
            entries: one("SELECT count(*) FROM entries")?,
            sections: one("SELECT count(*) FROM sections")?,
            by_source,
            by_raw_status: self.counts("SELECT raw_status, count(*) FROM entries GROUP BY 1 ORDER BY 1")?,
            by_summary_status: self.counts("SELECT summary_status, count(*) FROM entries GROUP BY 1 ORDER BY 1")?,
            by_summary_model: self.counts(
                "SELECT generator_kind || '/' || provider || '/' || model, count(*) FROM summaries GROUP BY 1 ORDER BY 2 DESC",
            )?,
            queue: one("SELECT count(*) FROM sync_queue")?,
        })
    }
}
