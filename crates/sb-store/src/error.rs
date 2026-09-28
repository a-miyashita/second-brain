use std::path::PathBuf;

/// Errors raised by the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("already exists: {0}")]
    AlreadyExists(String),
    #[error("invalid data: {0}")]
    Invalid(String),
    #[error(
        "the database schema (version {db}) is newer than this binary (version {binary}); upgrade second-brain"
    )]
    SchemaTooNew { db: i64, binary: i64 },
    #[error("home directory is not initialized: {0} (run `sb setup home`)")]
    NotInitialized(PathBuf),
    #[error("another sync is running (lock held: {0})")]
    Locked(PathBuf),
}

impl StoreError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        StoreError::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;
