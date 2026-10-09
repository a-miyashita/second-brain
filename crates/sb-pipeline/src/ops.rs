//! `sb refetch`, `sb reextract` and `sb index rebuild`.

use std::collections::BTreeMap;

use sb_store::fts::SqliteFts;
use sb_store::{Entry, EntryFilter, StoreError, SyncLock};
use second_brain_kernel::search::SearchBackend;
use second_brain_kernel::source::SourceError;
use second_brain_kernel::{
    EntryOrigin, FetchOutcome, FetchRequest, RawMode, RawRole, RawStatus, RunStatus, Severity,
    SourceKind,
};
use serde::Serialize;

use crate::Pipeline;
use crate::error::PipelineError;
use crate::host::{Host, Item};
use crate::policy::SummaryPolicy;
use crate::run::{Limits, Stop};

/// Result of refetch or reextract.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct OpReport {
    pub run_id: Option<i64>,
    pub selected: u64,
    pub updated: u64,
    pub unchanged: u64,
    pub not_applicable: u64,
    pub failed: u64,
    /// Entries skipped because their account has no source adapter or no raw data.
    pub skipped: u64,
    pub errors: Vec<String>,
    pub stop: Option<Stop>,
}

impl OpReport {
    fn status(&self) -> RunStatus {
        match &self.stop {
            Some(Stop::Cancelled) => RunStatus::Interrupted,
            Some(Stop::Limit(_)) => RunStatus::StoppedByLimit,
            None if self.failed > 0 => RunStatus::Partial,
            None => RunStatus::Ok,
        }
    }
}

fn lock(p: &Pipeline, holder: &str) -> Result<SyncLock, PipelineError> {
    match SyncLock::try_acquire(p.home(), holder) {
        Ok(l) => Ok(l),
        Err(StoreError::Locked(_)) => Err(PipelineError::Locked),
        Err(e) => Err(e.into()),
    }
}

fn group_by_account(entries: Vec<Entry>) -> BTreeMap<String, Vec<Entry>> {
    let mut m: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
    for e in entries {
        m.entry(e.account_id.clone()).or_default().push(e);
    }
    m
}

impl Pipeline {
    /// Fetch raw data again by natural key, replacing all segments.
    pub async fn refetch(
        &self,
        filter: &EntryFilter,
        limits: &Limits,
    ) -> Result<OpReport, PipelineError> {
        let _lock = lock(self, "refetch")?;
        let run_id = self.catalog().start_run("refetch", self.trigger)?;
        let policy = SummaryPolicy::load(&self.catalog())?;
        let entries = self.catalog().list_entries(filter)?;
        let mut rep = OpReport {
            run_id: Some(run_id),
            selected: entries.len() as u64,
            ..Default::default()
        };
        let batch: usize = self
            .catalog()
            .setting_or("pipeline.commit_batch", 20usize)?;
        'accounts: for (account_id, entries) in group_by_account(entries) {
            let account = self.account(&account_id)?;
            let Some(source) = self.source_for(&account)? else {
                rep.skipped += entries.len() as u64;
                continue;
            };
            let host = Host::new(self, account, source.clone(), &policy, EntryOrigin::Sync)?;
            for chunk in entries.chunks(batch.max(1)) {
                let mut items = Vec::new();
                for e in chunk {
                    if self.is_cancelled() {
                        rep.stop = Some(Stop::Cancelled);
                    } else if limits.time_exceeded() {
                        rep.stop = Some(Stop::Limit("--time-limit".into()));
                    }
                    if rep.stop.is_some() {
                        host.commit_items(std::mem::take(&mut items), vec![], vec![], vec![])?;
                        break 'accounts;
                    }
                    // An ingested document that kept its original keeps it.
                    let hint = if matches!(
                        e.source_kind,
                        SourceKind::GoogleDoc | SourceKind::WebPage | SourceKind::LocalFile
                    ) {
                        let keep = self
                            .catalog()
                            .raw_objects(e.id)?
                            .iter()
                            .any(|r| r.role == RawRole::Primary);
                        serde_json::json!({"keep_original": keep})
                    } else {
                        serde_json::Value::Null
                    };
                    let req = FetchRequest {
                        source_kind: e.source_kind,
                        source_id: e.source_id.clone(),
                        fetch_state: None,
                        metadata: e.metadata.clone(),
                        hint,
                        full: true,
                    };
                    match source.fetch(&host, &req).await {
                        Ok(FetchOutcome::Fetched(mut fe)) => {
                            fe.bundle.mode = RawMode::Replace;
                            items.push(Item {
                                fetched: *fe,
                                dequeue: None,
                            });
                            rep.updated += 1;
                        }
                        Ok(FetchOutcome::Unchanged) => rep.unchanged += 1,
                        Ok(FetchOutcome::NotApplicable(why)) => {
                            rep.not_applicable += 1;
                            rep.errors
                                .push(format!("{}: not applicable: {why}", e.entry_uid));
                        }
                        Ok(FetchOutcome::NotFound(why)) => {
                            self.refetch_failed(e, &why, &mut rep)?
                        }
                        Err(err @ SourceError::Auth(_)) => {
                            host.commit_items(std::mem::take(&mut items), vec![], vec![], vec![])?;
                            rep.errors.push(format!("{account_id}: {err}"));
                            rep.skipped += 1;
                            continue 'accounts;
                        }
                        Err(err) => self.refetch_failed(e, &err.to_string(), &mut rep)?,
                    }
                }
                host.commit_items(items, vec![], vec![], vec![])?;
            }
            if let Ok(errs) = host.errors.lock() {
                rep.failed += errs.len() as u64;
                rep.errors.extend(errs.iter().cloned());
            }
        }
        let stats = serde_json::to_value(&rep)?;
        self.catalog().finish_run(
            run_id,
            rep.status(),
            &stats,
            rep.errors.first().map(String::as_str),
        )?;
        Ok(rep)
    }

    fn refetch_failed(
        &self,
        e: &Entry,
        why: &str,
        rep: &mut OpReport,
    ) -> Result<(), PipelineError> {
        rep.failed += 1;
        rep.errors.push(format!("{}: {why}", e.entry_uid));
        let cat = self.catalog();
        if e.raw_status != RawStatus::Present {
            cat.set_raw_status(e.id, RawStatus::FetchFailed)?;
        }
        cat.open_issue(
            "raw.fetch_failed",
            Severity::Warning,
            Some(&e.account_id),
            Some(e.id),
            &format!("refetching \"{}\" failed: {why}", e.title),
        )?;
        Ok(())
    }

    /// Re-run `normalize` on stored raw data (no network).
    pub async fn reextract(
        &self,
        filter: &EntryFilter,
        limits: &Limits,
    ) -> Result<OpReport, PipelineError> {
        let run_id = self.catalog().start_run("reextract", self.trigger)?;
        let policy = SummaryPolicy::load(&self.catalog())?;
        let entries = self.catalog().list_entries(filter)?;
        let mut rep = OpReport {
            run_id: Some(run_id),
            selected: entries.len() as u64,
            ..Default::default()
        };
        'accounts: for (account_id, entries) in group_by_account(entries) {
            let account = self.account(&account_id)?;
            let Some(source) = self.source_for(&account)? else {
                rep.skipped += entries.len() as u64;
                continue;
            };
            let host = Host::new(self, account, source, &policy, EntryOrigin::Sync)?;
            for e in &entries {
                if self.is_cancelled() {
                    rep.stop = Some(Stop::Cancelled);
                    break 'accounts;
                }
                if limits.time_exceeded() {
                    rep.stop = Some(Stop::Limit("--time-limit".into()));
                    break 'accounts;
                }
                if e.raw_status != RawStatus::Present {
                    rep.skipped += 1;
                    continue;
                }
                let outcome = match host.normalize_stored(e) {
                    Ok(o) => o,
                    Err(err) => {
                        rep.failed += 1;
                        rep.errors.push(format!("{}: {err}", e.entry_uid));
                        continue;
                    }
                };
                match host.renormalized_update(e, outcome)? {
                    Some(u) => {
                        self.catalog().upsert_entry(&u)?;
                        rep.updated += 1;
                    }
                    None => rep.not_applicable += 1,
                }
            }
        }
        let stats = serde_json::to_value(&rep)?;
        self.catalog().finish_run(
            run_id,
            rep.status(),
            &stats,
            rep.errors.first().map(String::as_str),
        )?;
        Ok(rep)
    }

    /// Rebuild the active search backend(s). Returns the rows indexed.
    pub fn rebuild_index(&self) -> Result<u64, PipelineError> {
        let cat = self.catalog();
        let run_id = cat.start_run("index rebuild", self.trigger)?;
        let n = SqliteFts::new(&cat)
            .rebuild()
            .map_err(|e| PipelineError::Invalid(e.to_string()))?;
        cat.finish_run(run_id, RunStatus::Ok, &serde_json::json!({"rows": n}), None)?;
        Ok(n)
    }
}
