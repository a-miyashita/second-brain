//! Run records and issues (ADR-0009, ADR-0011).

use chrono::{DateTime, Utc};
use rusqlite::params;
use second_brain_kernel::{RunStatus, RunTrigger, Severity};
use serde::Serialize;
use serde_json::Value;

use crate::catalog::{Catalog, OptionalExt, opt_ts, parse_col, parse_json, req_ts};
use crate::error::Result;

/// A run row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Run {
    pub id: i64,
    pub command: String,
    pub trigger: RunTrigger,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: RunStatus,
    pub stats: Value,
    pub error: Option<String>,
}

/// An issue row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Issue {
    pub id: i64,
    pub code: String,
    pub severity: Severity,
    pub account_id: Option<String>,
    pub entry_id: Option<i64>,
    pub message: String,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub notified_at: Option<DateTime<Utc>>,
}

const RUN_COLS: &str = "id, command, trigger, started_at, finished_at, status, stats, error";
const ISSUE_COLS: &str = "id, code, severity, account_id, entry_id, message, first_seen_at, last_seen_at, resolved_at, notified_at";

fn run_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        id: r.get(0)?,
        command: r.get(1)?,
        trigger: parse_col(r.get(2)?, 2)?,
        started_at: req_ts(r.get(3)?),
        finished_at: opt_ts(r.get(4)?),
        status: parse_col(r.get(5)?, 5)?,
        stats: parse_json(r.get(6)?, 6)?,
        error: r.get(7)?,
    })
}

fn issue_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Issue> {
    Ok(Issue {
        id: r.get(0)?,
        code: r.get(1)?,
        severity: parse_col(r.get(2)?, 2)?,
        account_id: r.get(3)?,
        entry_id: r.get(4)?,
        message: r.get(5)?,
        first_seen_at: req_ts(r.get(6)?),
        last_seen_at: req_ts(r.get(7)?),
        resolved_at: opt_ts(r.get(8)?),
        notified_at: opt_ts(r.get(9)?),
    })
}

impl Catalog {
    pub fn start_run(&self, command: &str, trigger: RunTrigger) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO runs(command, trigger, started_at, status) VALUES (?1, ?2, ?3, 'running')",
            params![command, trigger.as_str(), self.now_ts()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_run(
        &self,
        id: i64,
        status: RunStatus,
        stats: &Value,
        error: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET finished_at = ?2, status = ?3, stats = ?4, error = ?5 WHERE id = ?1",
            params![id, self.now_ts(), status.as_str(), stats.to_string(), error],
        )?;
        Ok(())
    }

    /// Recent runs, newest first, optionally only for a command prefix and trigger.
    pub fn runs(
        &self,
        command_prefix: Option<&str>,
        trigger: Option<RunTrigger>,
        limit: u32,
    ) -> Result<Vec<Run>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {RUN_COLS} FROM runs
             WHERE (?1 IS NULL OR substr(command, 1, length(?1)) = ?1) AND (?2 IS NULL OR trigger = ?2)
             ORDER BY started_at DESC, id DESC LIMIT ?3"
        ))?;
        let rows = stmt
            .query_map(
                params![command_prefix, trigger.map(|t| t.as_str()), limit],
                run_from_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Mark runs left `running` by a killed process as `interrupted`.
    pub fn close_stale_runs(&self, command_prefix: &str, older_than: DateTime<Utc>) -> Result<u64> {
        let n = self.conn.execute(
            "UPDATE runs SET status = 'interrupted', finished_at = IFNULL(finished_at, started_at),
               error = IFNULL(error, 'process ended without finishing the run')
             WHERE status = 'running' AND started_at < ?1 AND substr(command, 1, length(?2)) = ?2",
            params![second_brain_kernel::util::ts(older_than), command_prefix],
        )?;
        Ok(n as u64)
    }

    /// Open or refresh an issue. Returns `true` if it is new.
    pub fn open_issue(
        &self,
        code: &str,
        severity: Severity,
        account_id: Option<&str>,
        entry_id: Option<i64>,
        message: &str,
    ) -> Result<bool> {
        let now = self.now_ts();
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM issues WHERE code = ?1 AND IFNULL(account_id, '') = IFNULL(?2, '')
                   AND IFNULL(entry_id, 0) = IFNULL(?3, 0) AND resolved_at IS NULL",
                params![code, account_id, entry_id],
                |r| r.get(0),
            )
            .opt()?;
        match existing {
            Some(id) => {
                self.conn.execute(
                    "UPDATE issues SET last_seen_at = ?2, message = ?3, severity = ?4 WHERE id = ?1",
                    params![id, now, message, severity.as_str()],
                )?;
                Ok(false)
            }
            None => {
                self.conn.execute(
                    "INSERT INTO issues(code, severity, account_id, entry_id, message, first_seen_at, last_seen_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                    params![code, severity.as_str(), account_id, entry_id, message, now],
                )?;
                Ok(true)
            }
        }
    }

    /// Resolve open issues matching a code (and account, when given). A code
    /// ending in `.` resolves every code with that prefix.
    pub fn resolve_issues(&self, code: &str, account_id: Option<&str>) -> Result<u64> {
        let n = self.conn.execute(
            "UPDATE issues SET resolved_at = ?3 WHERE resolved_at IS NULL
               AND (code = ?1 OR (substr(?1, -1) = '.' AND substr(code, 1, length(?1)) = ?1))
               AND (?2 IS NULL OR account_id = ?2)",
            params![code, account_id, self.now_ts()],
        )?;
        Ok(n as u64)
    }

    /// Resolve the open issues of one entry for a code.
    pub fn resolve_entry_issues(&self, code: &str, entry_id: i64) -> Result<u64> {
        let n = self.conn.execute(
            "UPDATE issues SET resolved_at = ?3 WHERE resolved_at IS NULL AND code = ?1 AND entry_id = ?2",
            params![code, entry_id, self.now_ts()],
        )?;
        Ok(n as u64)
    }

    pub fn open_issues(&self) -> Result<Vec<Issue>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ISSUE_COLS} FROM issues WHERE resolved_at IS NULL
             ORDER BY CASE severity WHEN 'error' THEN 0 WHEN 'warning' THEN 1 ELSE 2 END, last_seen_at DESC"
        ))?;
        let rows = stmt
            .query_map([], issue_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn issue(&self, id: i64) -> Result<Option<Issue>> {
        self.conn
            .query_row(
                &format!("SELECT {ISSUE_COLS} FROM issues WHERE id = ?1"),
                [id],
                issue_from_row,
            )
            .opt()
    }

    pub fn mark_issue_notified(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE issues SET notified_at = ?2 WHERE id = ?1",
            params![id, self.now_ts()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::test_util::temp_catalog;
    use serde_json::json;

    #[test]
    fn runs_lifecycle() {
        let (_d, cat) = temp_catalog();
        let id = cat.start_run("sync", RunTrigger::Schedule).unwrap();
        cat.finish_run(id, RunStatus::Ok, &json!({"new": 1}), None)
            .unwrap();
        let runs = cat
            .runs(Some("sync"), Some(RunTrigger::Schedule), 5)
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Ok);
        assert!(cat.runs(Some("import"), None, 5).unwrap().is_empty());
    }

    #[test]
    fn issues_are_unique_while_open() {
        let (_d, cat) = temp_catalog();
        assert!(
            cat.open_issue(
                "auth.needs_reauth",
                Severity::Error,
                Some("acme"),
                None,
                "a"
            )
            .unwrap()
        );
        assert!(
            !cat.open_issue(
                "auth.needs_reauth",
                Severity::Error,
                Some("acme"),
                None,
                "b"
            )
            .unwrap()
        );
        assert!(
            cat.open_issue(
                "auth.needs_reauth",
                Severity::Error,
                Some("other"),
                None,
                "c"
            )
            .unwrap()
        );
        assert_eq!(cat.open_issues().unwrap().len(), 2);
        assert_eq!(cat.resolve_issues("auth.", Some("acme")).unwrap(), 1);
        assert_eq!(cat.open_issues().unwrap().len(), 1);
        // Re-opening after resolution creates a new issue.
        assert!(
            cat.open_issue(
                "auth.needs_reauth",
                Severity::Error,
                Some("acme"),
                None,
                "d"
            )
            .unwrap()
        );
    }
}
