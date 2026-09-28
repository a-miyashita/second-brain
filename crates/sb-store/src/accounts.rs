//! Accounts (ADR-0007).

use chrono::{DateTime, Utc};
use rusqlite::{Row, params};
use sb_core::{AccountCtx, AccountId, AccountKind, AccountStatus};
use serde::Serialize;
use serde_json::Value;

use crate::catalog::{Catalog, OptionalExt, parse_col, parse_json, req_ts};
use crate::error::{Result, StoreError};

/// An account row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Account {
    pub id: AccountId,
    pub kind: AccountKind,
    pub label: String,
    pub identity: Option<String>,
    pub config: Value,
    pub status: AccountStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Account {
    pub fn ctx(&self) -> AccountCtx {
        AccountCtx {
            id: self.id.clone(),
            kind: self.kind,
            label: self.label.clone(),
            identity: self.identity.clone(),
            config: self.config.clone(),
        }
    }

    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        let id: String = r.get(0)?;
        Ok(Account {
            id: AccountId::new(id.clone()).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            kind: parse_col(r.get(1)?, 1)?,
            label: r.get(2)?,
            identity: r.get(3)?,
            config: parse_json(r.get(4)?, 4)?,
            status: parse_col(r.get(5)?, 5)?,
            created_at: req_ts(r.get(6)?),
            updated_at: req_ts(r.get(7)?),
        })
    }
}

const COLS: &str = "id, kind, label, identity, config, status, created_at, updated_at";

impl Catalog {
    /// Insert a new account. Fails if the ID exists.
    pub fn add_account(
        &self,
        id: &AccountId,
        kind: AccountKind,
        label: &str,
        identity: Option<&str>,
        config: &Value,
    ) -> Result<Account> {
        if self.account(id)?.is_some() {
            return Err(StoreError::AlreadyExists(format!("account {id}")));
        }
        let now = self.now_ts();
        self.conn.execute(
            "INSERT INTO accounts(id, kind, label, identity, config, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6)",
            params![id.as_str(), kind.as_str(), label, identity, config.to_string(), now],
        )?;
        self.account(id)?
            .ok_or_else(|| StoreError::NotFound(format!("account {id}")))
    }

    /// Insert an account if it does not exist (pseudo-accounts). Returns
    /// whether it was created.
    pub fn ensure_account(&self, id: &AccountId, kind: AccountKind, label: &str) -> Result<bool> {
        if self.account(id)?.is_some() {
            return Ok(false);
        }
        self.add_account(id, kind, label, None, &Value::Object(Default::default()))?;
        Ok(true)
    }

    pub fn account(&self, id: &AccountId) -> Result<Option<Account>> {
        self.conn
            .query_row(
                &format!("SELECT {COLS} FROM accounts WHERE id = ?1"),
                [id.as_str()],
                Account::from_row,
            )
            .opt()
    }

    /// Look up an account by a string ID; an invalid slug is "not found".
    pub fn account_by_str(&self, id: &str) -> Result<Option<Account>> {
        match AccountId::new(id) {
            Ok(id) => self.account(&id),
            Err(_) => Ok(None),
        }
    }

    pub fn accounts(&self) -> Result<Vec<Account>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {COLS} FROM accounts ORDER BY kind, id"))?;
        let rows = stmt
            .query_map([], Account::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn set_account_status(&self, id: &AccountId, status: AccountStatus) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE accounts SET status = ?2, updated_at = ?3 WHERE id = ?1",
            params![id.as_str(), status.as_str(), self.now_ts()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("account {id}")));
        }
        Ok(())
    }

    pub fn update_account(
        &self,
        id: &AccountId,
        label: Option<&str>,
        identity: Option<&str>,
        config: Option<&Value>,
    ) -> Result<()> {
        let now = self.now_ts();
        self.with_tx(|tx| {
            if let Some(l) = label {
                tx.execute(
                    "UPDATE accounts SET label = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id.as_str(), l, now],
                )?;
            }
            if let Some(i) = identity {
                tx.execute(
                    "UPDATE accounts SET identity = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id.as_str(), i, now],
                )?;
            }
            if let Some(c) = config {
                tx.execute(
                    "UPDATE accounts SET config = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id.as_str(), c.to_string(), now],
                )?;
            }
            Ok(())
        })
    }

    /// Remove an account. With `purge`, its entries (and index rows) are deleted
    /// too; the caller deletes raw files. Without `purge`, an account that
    /// still has entries cannot be removed.
    pub fn remove_account(&self, id: &AccountId, purge: bool) -> Result<u64> {
        let count: i64 = self.conn.query_row(
            "SELECT count(*) FROM entries WHERE account_id = ?1",
            [id.as_str()],
            |r| r.get(0),
        )?;
        if count > 0 && !purge {
            return Err(StoreError::Invalid(format!(
                "account {id} has {count} entries; use --purge to delete them too"
            )));
        }
        self.with_tx(|tx| {
            tx.execute(
                "DELETE FROM fts_sections WHERE rowid IN (SELECT s.id FROM sections s JOIN entries e ON e.id = s.entry_id WHERE e.account_id = ?1)",
                [id.as_str()],
            )?;
            tx.execute("DELETE FROM entries WHERE account_id = ?1", [id.as_str()])?;
            tx.execute("DELETE FROM secrets WHERE scope = ?1", [format!("account:{id}")])?;
            tx.execute("DELETE FROM cache WHERE account_id = ?1", [id.as_str()])?;
            tx.execute("DELETE FROM issues WHERE account_id = ?1", [id.as_str()])?;
            tx.execute("DELETE FROM accounts WHERE id = ?1", [id.as_str()])?;
            Ok(())
        })?;
        Ok(count as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::test_util::temp_catalog;

    #[test]
    fn account_crud() {
        let (_d, cat) = temp_catalog();
        let id = AccountId::new("acme-slack").unwrap();
        let a = cat
            .add_account(
                &id,
                AccountKind::Slack,
                "Acme",
                Some("T1:U1"),
                &serde_json::json!({"include_dms": true}),
            )
            .unwrap();
        assert_eq!(a.status, AccountStatus::Active);
        assert!(
            cat.add_account(&id, AccountKind::Slack, "x", None, &Value::Null)
                .is_err()
        );
        cat.set_account_status(&id, AccountStatus::NeedsReauth)
            .unwrap();
        assert_eq!(
            cat.account(&id).unwrap().unwrap().status,
            AccountStatus::NeedsReauth
        );
        assert_eq!(cat.accounts().unwrap().len(), 1);
        assert!(!cat.ensure_account(&id, AccountKind::Slack, "x").unwrap());
        cat.remove_account(&id, false).unwrap();
        assert!(cat.account(&id).unwrap().is_none());
    }
}
