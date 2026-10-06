//! One-time upgrade of stored input hashes to body-only hashes (ADR-0017).
//!
//! Before ADR-0017 `summaries.input_hash` covered the prompt version, the model
//! and the body. Shipping the new formula without this step would make every stored
//! hash stale and re-summarize the whole catalog. The step never calls an LLM and
//! never touches a summary, a section or `summary_status`.

use std::collections::BTreeMap;

use sb_core::util::body_hash;
use serde::Serialize;

use crate::policy::SummaryPolicy;
use crate::run::Progress;
use crate::{Pipeline, PipelineError};
use sb_store::Entry;

/// The setting that gates the upgrade: 1 = old hashes may exist, 2 = upgraded.
pub const HASH_VERSION_KEY: &str = "summary.input_hash_version";
const UPGRADED: i64 = 2;

/// What the upgrade did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct HashUpgradeReport {
    /// Summaries that had an old hash.
    pub total: u64,
    /// Rewritten from the rebuilt input.
    pub upgraded: u64,
    /// The input could not be rebuilt: the hash is now empty ("unknown baseline"),
    /// the summary is kept, and the next evaluation adopts a baseline.
    pub unknown: u64,
}

impl Pipeline {
    /// Run the hash upgrade if it has not completed. Returns `None` when there was
    /// nothing to do. Idempotent and resumable: rows already at `b2:` or marked
    /// unknown are skipped, and the marker is set only when every row was visited.
    pub fn upgrade_input_hashes(&self) -> Result<Option<HashUpgradeReport>, PipelineError> {
        if self.catalog().setting_or(HASH_VERSION_KEY, 1i64)? >= UPGRADED {
            return Ok(None);
        }
        let policy = SummaryPolicy::load(&self.catalog())?;
        let ids = self.catalog().summaries_with_legacy_hash()?;
        let mut report = HashUpgradeReport {
            total: ids.len() as u64,
            ..Default::default()
        };
        let mut by_account: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
        for id in ids {
            if let Some(e) = self.catalog().entry(id)? {
                by_account.entry(e.account_id.clone()).or_default().push(e);
            }
        }
        for entries in by_account.values() {
            // One account whose source cannot be built leaves its rows unknown; it
            // does not block the others.
            let hosts = match self.hosts(&policy, entries) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(error = %e, "hash upgrade: cannot build the source of an account");
                    Default::default()
                }
            };
            for e in entries {
                let hash = match self.rebuild_input(&hosts, e) {
                    Ok(Some(input)) => Some(body_hash(&input.body)),
                    Ok(None) => None,
                    Err(err) => {
                        tracing::debug!(entry = e.id, error = %err, "hash upgrade: input not rebuilt");
                        None
                    }
                };
                match hash {
                    Some(h) => {
                        self.catalog().set_summary_input_hash(e.id, &h)?;
                        report.upgraded += 1;
                    }
                    None => {
                        self.catalog().set_summary_input_hash(e.id, "")?;
                        report.unknown += 1;
                    }
                }
            }
        }
        self.catalog()
            .set_setting(HASH_VERSION_KEY, &serde_json::json!(UPGRADED))?;
        tracing::info!(?report, "summary input hashes upgraded");
        if report.total > 0 {
            self.emit(Progress::Stage(format!(
                "summary hash upgrade: {} upgraded, {} unknown (of {})",
                report.upgraded, report.unknown, report.total
            )));
        }
        Ok(Some(report))
    }
}
