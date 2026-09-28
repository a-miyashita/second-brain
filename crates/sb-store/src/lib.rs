//! SQLite catalog, raw file store, secrets, settings and the `sqlite-fts`
//! search backend.

pub mod accounts;
pub mod catalog;
pub mod entries;
pub mod error;
pub mod fts;
pub mod home;
pub mod lock;
pub mod perms;
pub mod rawstore;
pub mod runs;
pub mod secrets;
pub mod settings;
pub mod stats;
pub mod sync;

pub use accounts::Account;
pub use catalog::{Catalog, SCHEMA_VERSION};
pub use entries::{
    Entry, EntryFilter, EntryUpdate, NormalizedUpdate, RawObjectRow, Section, StoredRaw,
    SummaryCommit, SummaryDecision, SummaryRecord, UpsertResult,
};
pub use error::{Result, StoreError};
pub use home::Home;
pub use lock::SyncLock;
pub use runs::{Issue, Run};
pub use secrets::SecretScope;
pub use sync::{CommitBatch, QueueRow};
