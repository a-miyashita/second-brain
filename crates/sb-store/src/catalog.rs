//! The SQLite catalog (ADR-0003, data-model.md).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction};
use sb_core::clock::{Clock, SystemClock};
use sb_core::util::{parse_ts, ts};

use crate::error::{Result, StoreError};
use crate::home::Home;
use crate::perms;

/// Embedded, ordered migrations. Never edit an applied migration; add a new one.
const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_initial.sql")),
    (2, include_str!("../migrations/0002_budget.sql")),
    (3, include_str!("../migrations/0003_input_hash_version.sql")),
];

/// The schema version this binary expects.
pub const SCHEMA_VERSION: i64 = 3;

/// A catalog connection.
pub struct Catalog {
    pub(crate) conn: Connection,
    pub(crate) home: Home,
    pub(crate) clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Catalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalog").field("home", &self.home).finish()
    }
}

impl Catalog {
    /// Create the home layout and the database if needed, apply permissions
    /// and migrations. Idempotent (`sb setup home`).
    pub fn create(home: &Home) -> Result<Self> {
        home.ensure_layout()?;
        let cat = Self::open_inner(home, true, Arc::new(SystemClock))?;
        cat.secure_files()?;
        cat.migrate()?;
        Ok(cat)
    }

    /// Open an existing catalog and migrate it to the current schema.
    pub fn open(home: &Home) -> Result<Self> {
        Self::open_with_clock(home, Arc::new(SystemClock))
    }

    /// Open with an injected clock (tests).
    pub fn open_with_clock(home: &Home, clock: Arc<dyn Clock>) -> Result<Self> {
        if !home.is_initialized() {
            return Err(StoreError::NotInitialized(home.root().to_path_buf()));
        }
        let cat = Self::open_inner(home, false, clock)?;
        cat.migrate()?;
        Ok(cat)
    }

    /// Open without migrating (used by `doctor` to report the schema state).
    pub fn open_no_migrate(home: &Home) -> Result<Self> {
        if !home.is_initialized() {
            return Err(StoreError::NotInitialized(home.root().to_path_buf()));
        }
        Self::open_inner(home, false, Arc::new(SystemClock))
    }

    fn open_inner(home: &Home, create: bool, clock: Arc<dyn Clock>) -> Result<Self> {
        let path = home.db_path();
        let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        if create {
            flags |= OpenFlags::SQLITE_OPEN_CREATE;
        }
        let conn = Connection::open_with_flags(&path, flags)?;
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Ok(Catalog {
            conn,
            home: home.clone(),
            clock,
        })
    }

    /// Restrict the DB and its side files to the owner.
    pub fn secure_files(&self) -> Result<()> {
        let db = self.home.db_path();
        for suffix in ["", "-wal", "-shm"] {
            let p = db.with_file_name(format!("{}{suffix}", crate::home::DB_FILE));
            if p.exists() {
                perms::make_private_file(&p)?;
            }
        }
        Ok(())
    }

    pub fn home(&self) -> &Home {
        &self.home
    }

    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    pub fn set_clock(&mut self, clock: Arc<dyn Clock>) {
        self.clock = clock;
    }

    /// Current time as a catalog timestamp.
    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    pub(crate) fn now_ts(&self) -> String {
        ts(self.clock.now())
    }

    /// Raw access to the connection, for backends and diagnostics.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Run `f` in a transaction.
    pub fn with_tx<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let tx = self.conn.unchecked_transaction()?;
        let v = f(&tx)?;
        tx.commit()?;
        Ok(v)
    }

    /// The applied schema version (0 for an empty database).
    pub fn schema_version(&self) -> Result<i64> {
        let exists: bool = self.conn.query_row(
            "SELECT count(*) > 0 FROM sqlite_master WHERE type='table' AND name='schema_migrations'",
            [],
            |r| r.get(0),
        )?;
        if !exists {
            return Ok(0);
        }
        Ok(self.conn.query_row(
            "SELECT IFNULL(MAX(version), 0) FROM schema_migrations",
            [],
            |r| r.get(0),
        )?)
    }

    /// Apply pending migrations.
    pub fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
        )?;
        let current = self.schema_version()?;
        if current > SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew {
                db: current,
                binary: SCHEMA_VERSION,
            });
        }
        for (version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v > current) {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
                rusqlite::params![version, self.now_ts()],
            )?;
            tx.commit()?;
            tracing::info!(version, "applied migration");
        }
        Ok(())
    }

    /// `PRAGMA quick_check`; returns the problems found (empty when OK).
    pub fn quick_check(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("PRAGMA quick_check")?;
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows.into_iter().filter(|r| r != "ok").collect())
    }
}

/// Parse an optional stored timestamp.
pub(crate) fn opt_ts(s: Option<String>) -> Option<DateTime<Utc>> {
    s.as_deref().and_then(parse_ts)
}

/// Parse a required stored timestamp; falls back to the Unix epoch on corrupt data.
pub(crate) fn req_ts(s: String) -> DateTime<Utc> {
    parse_ts(&s).unwrap_or_default()
}

/// Parse a stored enum; unknown values are a data error.
pub(crate) fn parse_col<T: std::str::FromStr>(s: String, idx: usize) -> rusqlite::Result<T> {
    s.parse().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            idx,
            rusqlite::types::Type::Text,
            format!("unexpected value {s:?}").into(),
        )
    })
}

/// Parse a stored JSON column.
pub(crate) fn parse_json(s: String, idx: usize) -> rusqlite::Result<serde_json::Value> {
    serde_json::from_str(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
    })
}

pub(crate) trait OptionalExt<T> {
    fn opt(self) -> Result<Option<T>>;
}

impl<T> OptionalExt<T> for rusqlite::Result<T> {
    fn opt(self) -> Result<Option<T>> {
        Ok(self.optional()?)
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use super::*;

    /// A catalog in a temporary home. Keep the `TempDir` alive.
    pub fn temp_catalog() -> (tempfile::TempDir, Catalog) {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path().join("home"));
        let cat = Catalog::create(&home).unwrap();
        (dir, cat)
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::temp_catalog;
    use super::*;

    #[test]
    fn create_is_idempotent_and_migrated() {
        let (_d, cat) = temp_catalog();
        assert_eq!(cat.schema_version().unwrap(), SCHEMA_VERSION);
        let again = Catalog::create(cat.home()).unwrap();
        assert_eq!(again.schema_version().unwrap(), SCHEMA_VERSION);
        assert!(cat.quick_check().unwrap().is_empty());
    }

    #[test]
    fn open_requires_init() {
        let dir = tempfile::tempdir().unwrap();
        let err = Catalog::open(&Home::new(dir.path().join("nope"))).unwrap_err();
        assert!(matches!(err, StoreError::NotInitialized(_)));
    }

    #[cfg(unix)]
    #[test]
    fn files_are_private() {
        let (_d, cat) = temp_catalog();
        assert_eq!(
            perms::is_private(cat.home().root(), true).unwrap(),
            Some(true)
        );
        assert_eq!(
            perms::is_private(&cat.home().db_path(), false).unwrap(),
            Some(true)
        );
    }
}
