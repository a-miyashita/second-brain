//! Sync state: cursors, the work queue, the cache, and atomic commit batches
//! (ADR-0012).

use chrono::{DateTime, Duration, Utc};
use rusqlite::params;
use sb_core::util::ts;
use sb_core::{AccountId, CursorUpdate, QueueItem, SourceKind};
use serde::Serialize;
use serde_json::Value;

use crate::catalog::{Catalog, OptionalExt, parse_col, parse_json, req_ts};
use crate::entries::{EntryUpdate, UpsertResult, upsert_entry_tx};
use crate::error::Result;

/// A queued item.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueueRow {
    pub account_id: String,
    pub source_kind: SourceKind,
    pub source_id: String,
    pub reason: String,
    pub hint: Value,
    pub enqueued_at: DateTime<Utc>,
    pub attempts: i64,
    pub last_error: Option<String>,
}

/// Everything committed in one transaction: entries, queue changes and the
/// cursors that cover them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CommitBatch {
    pub account_id: Option<AccountId>,
    pub entries: Vec<EntryUpdate>,
    pub enqueue: Vec<QueueItem>,
    /// Queue rows to delete: (source_kind, source_id).
    pub dequeue: Vec<(SourceKind, String)>,
    pub cursors: Vec<CursorUpdate>,
}

impl Catalog {
    /// Commit a batch atomically. Returns one result per entry.
    pub fn commit_batch(&self, b: &CommitBatch) -> Result<Vec<UpsertResult>> {
        let now = self.now_ts();
        self.with_tx(|tx| {
            let mut results = Vec::with_capacity(b.entries.len());
            for e in &b.entries {
                results.push(upsert_entry_tx(tx, &now, e)?);
            }
            if let Some(acct) = &b.account_id {
                for q in &b.enqueue {
                    tx.execute(
                        "INSERT INTO sync_queue(account_id, source_kind, source_id, reason, hint, enqueued_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                         ON CONFLICT(account_id, source_kind, source_id) DO UPDATE SET hint = excluded.hint",
                        params![acct.as_str(), q.source_kind.as_str(), q.source_id, q.reason, q.hint.to_string(), now],
                    )?;
                }
                for (kind, id) in &b.dequeue {
                    tx.execute(
                        "DELETE FROM sync_queue WHERE account_id = ?1 AND source_kind = ?2 AND source_id = ?3",
                        params![acct.as_str(), kind.as_str(), id],
                    )?;
                }
                for c in &b.cursors {
                    match &c.value {
                        Some(v) => tx.execute(
                            "INSERT INTO sync_state(account_id, source_kind, key, value, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
                             ON CONFLICT(account_id, source_kind, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                            params![acct.as_str(), c.source_kind.as_str(), c.key, v.to_string(), now],
                        )?,
                        None => tx.execute(
                            "DELETE FROM sync_state WHERE account_id = ?1 AND source_kind = ?2 AND key = ?3",
                            params![acct.as_str(), c.source_kind.as_str(), c.key],
                        )?,
                    };
                }
            }
            Ok(results)
        })
    }

    pub fn cursor(
        &self,
        account: &AccountId,
        kind: SourceKind,
        key: &str,
    ) -> Result<Option<Value>> {
        let v: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM sync_state WHERE account_id = ?1 AND source_kind = ?2 AND key = ?3",
                params![account.as_str(), kind.as_str(), key],
                |r| r.get(0),
            )
            .opt()?;
        Ok(v.map(|s| serde_json::from_str(&s)).transpose()?)
    }

    pub fn cursors(
        &self,
        account: &AccountId,
        kind: SourceKind,
        prefix: &str,
    ) -> Result<Vec<(String, Value)>> {
        let mut stmt = self.conn.prepare(
            "SELECT key, value FROM sync_state WHERE account_id = ?1 AND source_kind = ?2
               AND substr(key, 1, length(?3)) = ?3 ORDER BY key",
        )?;
        let rows = stmt
            .query_map(params![account.as_str(), kind.as_str(), prefix], |r| {
                Ok((r.get::<_, String>(0)?, parse_json(r.get(1)?, 1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Queue rows of an account (optionally for some kinds), oldest first.
    pub fn queue(
        &self,
        account: Option<&AccountId>,
        kinds: &[SourceKind],
    ) -> Result<Vec<QueueRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT account_id, source_kind, source_id, reason, hint, enqueued_at, attempts, last_error
             FROM sync_queue WHERE (?1 IS NULL OR account_id = ?1) ORDER BY enqueued_at, source_id",
        )?;
        let rows = stmt
            .query_map([account.map(|a| a.as_str().to_string())], |r| {
                Ok(QueueRow {
                    account_id: r.get(0)?,
                    source_kind: parse_col(r.get(1)?, 1)?,
                    source_id: r.get(2)?,
                    reason: r.get(3)?,
                    hint: parse_json(r.get(4)?, 4)?,
                    enqueued_at: req_ts(r.get(5)?),
                    attempts: r.get(6)?,
                    last_error: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .filter(|q| kinds.is_empty() || kinds.contains(&q.source_kind))
            .collect())
    }

    /// Record a failed fetch attempt for a queued item.
    pub fn queue_failure(
        &self,
        account: &AccountId,
        kind: SourceKind,
        id: &str,
        error: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE sync_queue SET attempts = attempts + 1, last_error = ?4
             WHERE account_id = ?1 AND source_kind = ?2 AND source_id = ?3",
            params![account.as_str(), kind.as_str(), id, error],
        )?;
        Ok(())
    }

    pub fn cache_get(&self, account: &str, key: &str) -> Result<Option<Value>> {
        let v: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT value, expires_at FROM cache WHERE account_id = ?1 AND key = ?2",
                params![account, key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .opt()?;
        match v {
            Some((val, exp)) if req_ts(exp.clone()) > self.now() => {
                Ok(Some(serde_json::from_str(&val)?))
            }
            _ => Ok(None),
        }
    }

    /// Read a cache value even if expired (e.g. a stale user directory is
    /// better than none for normalization).
    pub fn cache_get_stale(&self, account: &str, key: &str) -> Result<Option<Value>> {
        let v: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM cache WHERE account_id = ?1 AND key = ?2",
                params![account, key],
                |r| r.get(0),
            )
            .opt()?;
        Ok(v.map(|s| serde_json::from_str(&s)).transpose()?)
    }

    pub fn cache_put(&self, account: &str, key: &str, value: &Value, ttl: Duration) -> Result<()> {
        self.conn.execute(
            "INSERT INTO cache(account_id, key, value, expires_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(account_id, key) DO UPDATE SET value = excluded.value, expires_at = excluded.expires_at",
            params![account, key, value.to_string(), ts(self.now() + ttl)],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::test_util::temp_catalog;
    use sb_core::AccountKind;
    use serde_json::json;

    #[test]
    fn batch_commits_queue_and_cursors_together() {
        let (_d, cat) = temp_catalog();
        let a = AccountId::new("acme").unwrap();
        cat.ensure_account(&a, AccountKind::Slack, "Acme").unwrap();
        cat.commit_batch(&CommitBatch {
            account_id: Some(a.clone()),
            entries: vec![],
            enqueue: vec![QueueItem {
                source_kind: SourceKind::SlackThread,
                source_id: "C1:1.0".into(),
                reason: "new_thread".into(),
                hint: json!({"latest_reply": "2.0"}),
            }],
            dequeue: vec![],
            cursors: vec![CursorUpdate {
                source_kind: SourceKind::SlackThread,
                key: "conv:C1".into(),
                value: Some(json!({"oldest": "1.0"})),
            }],
        })
        .unwrap();
        assert_eq!(cat.queue(Some(&a), &[]).unwrap().len(), 1);
        assert_eq!(
            cat.cursor(&a, SourceKind::SlackThread, "conv:C1").unwrap(),
            Some(json!({"oldest": "1.0"}))
        );
        assert_eq!(
            cat.cursors(&a, SourceKind::SlackThread, "conv:")
                .unwrap()
                .len(),
            1
        );
        cat.queue_failure(&a, SourceKind::SlackThread, "C1:1.0", "boom")
            .unwrap();
        assert_eq!(
            cat.queue(Some(&a), &[SourceKind::SlackThread]).unwrap()[0].attempts,
            1
        );
        cat.commit_batch(&CommitBatch {
            account_id: Some(a.clone()),
            dequeue: vec![(SourceKind::SlackThread, "C1:1.0".into())],
            cursors: vec![CursorUpdate {
                source_kind: SourceKind::SlackThread,
                key: "conv:C1".into(),
                value: None,
            }],
            ..Default::default()
        })
        .unwrap();
        assert!(cat.queue(Some(&a), &[]).unwrap().is_empty());
        assert!(
            cat.cursor(&a, SourceKind::SlackThread, "conv:C1")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn cache_expiry() {
        let (_d, cat) = temp_catalog();
        cat.cache_put("acme", "users", &json!([1]), Duration::days(1))
            .unwrap();
        assert_eq!(cat.cache_get("acme", "users").unwrap(), Some(json!([1])));
        cat.cache_put("acme", "users", &json!([2]), Duration::days(-1))
            .unwrap();
        assert_eq!(cat.cache_get("acme", "users").unwrap(), None);
        assert_eq!(
            cat.cache_get_stale("acme", "users").unwrap(),
            Some(json!([2]))
        );
    }
}
