//! The source adapter model (ADR-0008).

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::kinds::{AccountKind, SourceKind};
use crate::model::{
    DiscoveryBatch, FetchOutcome, FetchRequest, NormalizeCtx, NormalizeInput, NormalizeOutcome,
    SourceRef, SyncOptions,
};

/// Errors raised by source adapters.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// Credentials are invalid or revoked; the account needs re-authentication.
    #[error("authentication failed: {0}")]
    Auth(String),
    /// The API asked us to back off, and retrying did not help.
    #[error("rate limited: {0}")]
    RateLimited(String),
    /// A transport-level error.
    #[error("network error: {0}")]
    Network(String),
    /// The API returned an error.
    #[error("API error: {0}")]
    Api(String),
    /// The data could not be parsed.
    #[error("parse error: {0}")]
    Parse(String),
    /// The run was cancelled.
    #[error("cancelled")]
    Cancelled,
    /// The host (catalog) failed.
    #[error("host error: {0}")]
    Host(String),
    /// The operation is not supported by this source.
    #[error("unsupported: {0}")]
    Unsupported(String),
}

impl SourceError {
    /// Stable issue code for this error.
    pub fn issue_code(&self) -> &'static str {
        match self {
            SourceError::Auth(_) => "auth.needs_reauth",
            SourceError::RateLimited(_) => "sync.rate_limited",
            SourceError::Network(_) => "sync.network",
            SourceError::Api(_) => "sync.api_error",
            SourceError::Parse(_) => "sync.parse_error",
            SourceError::Cancelled => "sync.cancelled",
            SourceError::Host(_) => "sync.host_error",
            SourceError::Unsupported(_) => "sync.unsupported",
        }
    }
}

/// Services the pipeline offers to sources during sync and fetch. All reads and
/// writes are scoped to the source's account.
pub trait SyncHost: Send + Sync {
    /// Read a cursor from `sync_state`.
    fn cursor(&self, kind: SourceKind, key: &str) -> Result<Option<Value>, SourceError>;
    /// All cursors of a source kind whose key starts with `prefix`.
    fn cursors(&self, kind: SourceKind, prefix: &str) -> Result<Vec<(String, Value)>, SourceError>;
    /// The stored fetch state of an entry, if it exists.
    fn fetch_state(&self, kind: SourceKind, source_id: &str) -> Result<Option<Value>, SourceError>;
    /// Whether an entry exists (in any raw status).
    fn entry_exists(&self, kind: SourceKind, source_id: &str) -> Result<bool, SourceError>;
    /// Commit a discovery batch atomically.
    fn commit(&self, batch: DiscoveryBatch) -> Result<(), SourceError>;
    /// Read an unexpired cache value.
    fn cache_get(&self, key: &str) -> Result<Option<Value>, SourceError>;
    /// Store a cache value.
    fn cache_put(&self, key: &str, value: &Value, ttl: Duration) -> Result<(), SourceError>;
    /// Whether the run is being stopped. Sources check this between units of work.
    fn is_cancelled(&self) -> bool;
    /// The current time (from the injected clock).
    fn now(&self) -> DateTime<Utc>;
}

/// A source adapter bound to one account.
#[async_trait]
pub trait Source: Send + Sync {
    /// Source kinds this adapter produces (e.g. Slack produces threads and days).
    fn kinds(&self) -> &'static [SourceKind];

    fn account_kind(&self) -> AccountKind;

    /// Map a URL or path to a natural key, for single-item ingest.
    fn resolve(&self, _locator: &str) -> Option<SourceRef> {
        None
    }

    /// Whether `sync` is implemented.
    fn supports_sync(&self) -> bool {
        true
    }

    /// Incremental discovery. Commits discovery batches through the host and
    /// returns when the source is exhausted or the host is cancelled.
    async fn sync(&self, host: &dyn SyncHost, opts: &SyncOptions) -> Result<(), SourceError>;

    /// Fetch one item. With a fetch state and `full = false`, only the new part
    /// is fetched where the source supports appending.
    async fn fetch(
        &self,
        host: &dyn SyncHost,
        req: &FetchRequest,
    ) -> Result<FetchOutcome, SourceError>;

    /// Load the snapshot `normalize` needs (e.g. a user directory) from the cache.
    fn load_snapshot(&self, _host: &dyn SyncHost) -> Result<Value, SourceError> {
        Ok(Value::Null)
    }

    /// Pure, deterministic normalization of stored raw data. Must not touch the
    /// network or the clock.
    fn normalize(
        &self,
        ctx: &NormalizeCtx,
        input: &NormalizeInput,
    ) -> Result<NormalizeOutcome, SourceError>;
}
