//! Run bookkeeping: limits, stop reasons, statistics and progress events.

use std::collections::BTreeMap;
use std::time::Instant;

use sb_core::RunStatus;
use serde::Serialize;

/// Limits that stop a run cleanly (ADR-0012).
#[derive(Debug, Clone, Default)]
pub struct Limits {
    pub max_summaries: Option<u64>,
    pub max_cost_usd: Option<f64>,
    pub deadline: Option<Instant>,
}

impl Limits {
    pub fn time_exceeded(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d)
    }
}

/// Why a stage stopped before finishing.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum Stop {
    Limit(String),
    Cancelled,
}

/// Counts per account and source kind.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SourceStats {
    pub new: u64,
    pub updated: u64,
    pub unchanged: u64,
    pub not_applicable: u64,
    pub failed: u64,
    pub queued: u64,
}

/// Summarization counts.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SummaryStats {
    pub summarized: u64,
    pub failed: u64,
    pub skipped: u64,
    pub no_raw: u64,
    pub already_current: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    /// Summaries whose cost could not be estimated (no price known).
    pub unpriced: u64,
    pub remaining: u64,
}

/// Statistics of a run, stored as JSON in `runs.stats`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RunStats {
    /// Keyed by `"<account>/<source_kind>"`.
    pub sources: BTreeMap<String, SourceStats>,
    pub summaries: SummaryStats,
    pub errors: Vec<String>,
    pub stop: Option<Stop>,
    /// Remaining work: queued fetches and pending summaries.
    pub queue_remaining: u64,
    pub pending_summaries: u64,
}

impl RunStats {
    pub fn source(&mut self, account: &str, kind: &str) -> &mut SourceStats {
        self.sources.entry(format!("{account}/{kind}")).or_default()
    }

    pub fn committed_entries(&self) -> u64 {
        self.sources.values().map(|s| s.new + s.updated).sum()
    }

    /// The run status implied by the stats.
    pub fn status(&self) -> RunStatus {
        match &self.stop {
            Some(Stop::Cancelled) => RunStatus::Interrupted,
            Some(Stop::Limit(_)) if self.errors.is_empty() => RunStatus::StoppedByLimit,
            _ if !self.errors.is_empty() => RunStatus::Partial,
            Some(Stop::Limit(_)) => RunStatus::StoppedByLimit,
            None => RunStatus::Ok,
        }
    }
}

/// A progress event for interactive output (printed to stderr by the CLI).
#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    Stage(String),
    Fetched {
        account: String,
        committed: u64,
        queued: u64,
    },
    Summarized {
        done: u64,
        remaining: u64,
        cost_usd: f64,
    },
    Warning(String),
}

/// Callback for progress events.
pub type ProgressFn = std::sync::Arc<dyn Fn(Progress) + Send + Sync>;
