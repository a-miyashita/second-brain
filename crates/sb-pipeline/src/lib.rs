//! The ingestion and summarization pipeline (ADR-0008, ADR-0012):
//! fetch → store raw → normalize → upsert → summarize → index.

pub mod budget;
pub mod error;
pub mod hash_upgrade;
mod host;
pub mod import;
pub mod ingest;
pub mod ops;
pub mod policy;
pub mod run;
pub mod summarize;
pub mod sync;

use std::sync::{Arc, Mutex, MutexGuard};

use sb_core::RunTrigger;
use sb_core::clock::Clock;
use sb_core::source::Source;
use sb_store::{Account, Catalog, Home};
use tokio_util::sync::CancellationToken;

pub use error::PipelineError;
pub use run::{Limits, Progress, ProgressFn, RunStats, Stop};

/// Builds source adapters for accounts. Implemented by the CLI, which knows
/// every source crate.
pub trait SourceFactory: Send + Sync {
    /// The adapter for an account, or `None` if its kind has no sources yet.
    fn source(
        &self,
        account: &Account,
        catalog: &Catalog,
    ) -> Result<Option<Arc<dyn Source>>, PipelineError>;
}

/// Shared state of a pipeline run.
pub struct Pipeline {
    catalog: Mutex<Catalog>,
    home: Home,
    clock: Arc<dyn Clock>,
    factory: Arc<dyn SourceFactory>,
    /// Triggered on the first interrupt signal: no new work is started.
    pub cancel: CancellationToken,
    pub trigger: RunTrigger,
    progress: Option<ProgressFn>,
    /// The run recorded in the usage ledger (0 = none).
    run_id: std::sync::atomic::AtomicI64,
}

impl Pipeline {
    pub fn new(catalog: Catalog, factory: Arc<dyn SourceFactory>) -> Self {
        let home = catalog.home().clone();
        let clock = catalog.clock().clone();
        Pipeline {
            catalog: Mutex::new(catalog),
            home,
            clock,
            factory,
            cancel: CancellationToken::new(),
            trigger: RunTrigger::Manual,
            progress: None,
            run_id: std::sync::atomic::AtomicI64::new(0),
        }
    }

    /// Record the run that the following paid calls belong to (usage ledger).
    pub(crate) fn set_run_id(&self, id: i64) {
        self.run_id.store(id, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn run_id(&self) -> Option<i64> {
        match self.run_id.load(std::sync::atomic::Ordering::SeqCst) {
            0 => None,
            id => Some(id),
        }
    }

    pub fn with_progress(mut self, f: ProgressFn) -> Self {
        self.progress = Some(f);
        self
    }

    pub fn with_trigger(mut self, t: RunTrigger) -> Self {
        self.trigger = t;
        self
    }

    /// Lock the catalog. Never hold the guard across an `.await`.
    pub fn catalog(&self) -> MutexGuard<'_, Catalog> {
        match self.catalog.lock() {
            Ok(g) => g,
            // A panic while holding the lock leaves SQLite consistent (the
            // transaction is rolled back), so the connection is still usable.
            Err(p) => p.into_inner(),
        }
    }

    pub fn home(&self) -> &Home {
        &self.home
    }

    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub(crate) fn emit(&self, p: Progress) {
        if let Some(f) = &self.progress {
            f(p);
        }
    }

    /// The source adapter of an account.
    pub fn source_for(&self, account: &Account) -> Result<Option<Arc<dyn Source>>, PipelineError> {
        let cat = self.catalog();
        self.factory.source(account, &cat)
    }

    pub fn account(&self, id: &str) -> Result<Account, PipelineError> {
        let cat = self.catalog();
        cat.account_by_str(id)?
            .ok_or_else(|| PipelineError::Invalid(format!("unknown account {id:?}")))
    }
}
