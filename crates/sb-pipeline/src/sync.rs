//! `sb sync`: incremental, resumable sync of all enabled accounts and sources,
//! then pending summaries (ADR-0008, ADR-0012).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use sb_core::source::{Source, SourceError};
use sb_core::{
    AccountStatus, EntryOrigin, FetchOutcome, FetchRequest, RunStatus, Severity, SourceKind,
    SyncMode, SyncOptions as SourceSyncOptions,
};
use sb_store::{Account, EntryFilter, QueueRow, StoreError, SyncLock};
use serde::Serialize;
use serde_json::Value;

use crate::Pipeline;
use crate::error::PipelineError;
use crate::host::{Host, Item};
use crate::policy::SummaryPolicy;
use crate::run::{Limits, Progress, RunStats, Stop};
use crate::summarize::{SummarizeOptions, SummarizeReport, Target};

/// Queue items at this many failed attempts are left for `doctor`.
pub const MAX_QUEUE_ATTEMPTS: i64 = 5;

/// Options of `sb sync`.
#[derive(Debug, Clone, Default)]
pub struct SyncOptions {
    pub accounts: Vec<String>,
    pub sources: Vec<SourceKind>,
    pub mode: SyncMode,
    pub since: Option<DateTime<Utc>>,
    pub no_summary: bool,
    pub limits: Limits,
    pub dry_run: bool,
    pub estimate: bool,
}

/// What a dry run would do.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DryRun {
    pub accounts: Vec<DryRunAccount>,
    pub pending_summaries: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DryRunAccount {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub sources: Vec<String>,
    pub queued: u64,
    pub skipped_reason: Option<String>,
}

/// Result of `sb sync`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncReport {
    pub run_id: Option<i64>,
    pub status: Option<RunStatus>,
    pub stats: RunStats,
    pub summarize: Option<SummarizeReport>,
    pub dry_run: Option<DryRun>,
}

impl Pipeline {
    fn selected_accounts(&self, opts: &SyncOptions) -> Result<Vec<Account>, PipelineError> {
        let all = self.catalog().accounts()?;
        for want in &opts.accounts {
            if !all.iter().any(|a| a.id.as_str() == want) {
                return Err(PipelineError::Invalid(format!("unknown account {want:?}")));
            }
        }
        Ok(all
            .into_iter()
            .filter(|a| {
                opts.accounts.is_empty() || opts.accounts.iter().any(|w| w == a.id.as_str())
            })
            .collect())
    }

    /// Run `sb sync`.
    pub async fn sync(&self, opts: &SyncOptions) -> Result<SyncReport, PipelineError> {
        if opts.dry_run {
            return self.sync_dry_run(opts);
        }
        if opts.estimate {
            let report = self
                .summarize_pending(&SummarizeOptions {
                    filter: EntryFilter {
                        accounts: opts.accounts.clone(),
                        source_kinds: opts.sources.clone(),
                        ..Default::default()
                    },
                    target: Target::Configured,
                    limits: Limits::default(),
                    retry_failed: false,
                    force: false,
                    estimate_only: true,
                })
                .await?;
            return Ok(SyncReport {
                summarize: Some(report),
                ..Default::default()
            });
        }
        let _lock = match SyncLock::try_acquire(self.home(), "sync") {
            Ok(l) => l,
            Err(StoreError::Locked(_)) => return Err(PipelineError::Locked),
            Err(e) => return Err(e.into()),
        };
        let command = if opts.mode == SyncMode::Deep {
            "sync --deep"
        } else {
            "sync"
        };
        let run_id = {
            let cat = self.catalog();
            // We hold the lock, so any other "running" sync run was killed.
            cat.close_stale_runs("sync", cat.now())?;
            cat.start_run(command, self.trigger)?
        };
        self.set_run_id(run_id);
        let mut stats = RunStats::default();
        let result = self.sync_inner(opts, &mut stats).await;
        let mut summarize = None;
        if let Err(e) = &result {
            stats.errors.push(e.to_string());
        } else if !opts.no_summary && stats.stop.is_none() {
            self.emit(Progress::Stage("summarize".into()));
            let r = self
                .summarize_pending(&SummarizeOptions {
                    filter: EntryFilter {
                        accounts: opts.accounts.clone(),
                        source_kinds: opts.sources.clone(),
                        ..Default::default()
                    },
                    target: Target::Configured,
                    limits: opts.limits.clone(),
                    retry_failed: false,
                    force: false,
                    estimate_only: false,
                })
                .await?;
            stats.summaries = r.stats.clone();
            stats.errors.extend(r.errors.iter().cloned());
            if stats.stop.is_none() {
                stats.stop = r.stop.clone();
            }
            summarize = Some(r);
        }
        {
            let cat = self.catalog();
            stats.queue_remaining = cat.queue(None, &[])?.len() as u64;
            let policy = SummaryPolicy::load(&cat)?;
            stats.pending_summaries = cat
                .summarization_candidates(&EntryFilter::default(), policy.max_attempts)?
                .len() as u64;
        }
        let status = if result.is_err() && stats.committed_entries() == 0 {
            RunStatus::Failed
        } else {
            stats.status()
        };
        let stats_json = serde_json::to_value(&stats)?;
        self.catalog().finish_run(
            run_id,
            status,
            &stats_json,
            stats.errors.first().map(String::as_str),
        )?;
        if result.is_err() && status == RunStatus::Failed {
            result?;
        }
        Ok(SyncReport {
            run_id: Some(run_id),
            status: Some(status),
            stats,
            summarize,
            dry_run: None,
        })
    }

    async fn sync_inner(
        &self,
        opts: &SyncOptions,
        stats: &mut RunStats,
    ) -> Result<(), PipelineError> {
        let policy = SummaryPolicy::load(&self.catalog())?;
        for account in self.selected_accounts(opts)? {
            if self.is_cancelled() {
                stats.stop = Some(Stop::Cancelled);
                break;
            }
            if opts.limits.time_exceeded() {
                stats.stop = Some(Stop::Limit("--time-limit".into()));
                break;
            }
            match account.status {
                AccountStatus::Disabled => continue,
                AccountStatus::NeedsReauth => {
                    stats.errors.push(format!(
                        "{}: needs re-authentication (run `sb auth login {}`)",
                        account.id, account.id
                    ));
                    continue;
                }
                AccountStatus::Active => {}
            }
            let Some(source) = self.source_for(&account)? else {
                continue;
            };
            if !source.supports_sync() {
                continue;
            }
            let kinds: Vec<SourceKind> = source
                .kinds()
                .iter()
                .copied()
                .filter(|k| opts.sources.is_empty() || opts.sources.contains(k))
                .collect();
            if kinds.is_empty() {
                continue;
            }
            self.emit(Progress::Stage(format!("sync {}", account.id)));
            let host = Host::new(
                self,
                account.clone(),
                source.clone(),
                &policy,
                EntryOrigin::Sync,
            )?;
            let res = self
                .sync_account(&host, source.as_ref(), &kinds, opts)
                .await;
            // Merge the host's counters.
            if let Ok(s) = host.stats.lock() {
                for (kind, st) in s.iter() {
                    let t = stats.source(account.id.as_str(), kind.as_str());
                    t.new += st.new;
                    t.updated += st.updated;
                    t.unchanged += st.unchanged;
                    t.not_applicable += st.not_applicable;
                    t.failed += st.failed;
                    t.queued += st.queued;
                }
            }
            if let Ok(e) = host.errors.lock() {
                stats.errors.extend(e.iter().cloned());
            }
            let cat = self.catalog();
            match res {
                Ok(stop) => {
                    let now = serde_json::json!(sb_core::util::ts(cat.now()));
                    cat.cache_put(
                        account.id.as_str(),
                        "auth.last_ok",
                        &now,
                        chrono::Duration::days(3650),
                    )?;
                    cat.resolve_issues("auth.", Some(account.id.as_str()))?;
                    cat.resolve_issues("sync.", Some(account.id.as_str()))?;
                    if let Some(s) = stop {
                        stats.stop = Some(s);
                        break;
                    }
                }
                Err(PipelineError::Source(SourceError::Cancelled)) => {
                    stats.stop = Some(Stop::Cancelled);
                    break;
                }
                Err(PipelineError::Source(e @ SourceError::Auth(_))) => {
                    cat.set_account_status(&account.id, AccountStatus::NeedsReauth)?;
                    let msg = format!("{e}; run `sb auth login {}`", account.id);
                    cat.open_issue(
                        e.issue_code(),
                        Severity::Error,
                        Some(account.id.as_str()),
                        None,
                        &msg,
                    )?;
                    stats.errors.push(format!("{}: {msg}", account.id));
                }
                Err(PipelineError::Source(e)) => {
                    let msg = e.to_string();
                    cat.open_issue(
                        e.issue_code(),
                        Severity::Warning,
                        Some(account.id.as_str()),
                        None,
                        &msg,
                    )?;
                    stats.errors.push(format!("{}: {msg}", account.id));
                }
                Err(e) => return Err(e),
            }
        }
        if stats.stop.is_none() && self.is_cancelled() {
            stats.stop = Some(Stop::Cancelled);
        }
        Ok(())
    }

    /// Drain the queue, discover, and drain again.
    async fn sync_account(
        &self,
        host: &Host<'_>,
        source: &dyn Source,
        kinds: &[SourceKind],
        opts: &SyncOptions,
    ) -> Result<Option<Stop>, PipelineError> {
        if let Some(stop) = self.drain_queue(host, kinds, &opts.limits).await? {
            return Ok(Some(stop));
        }
        let sopts = SourceSyncOptions {
            mode: opts.mode,
            since: opts.since,
            kinds: kinds.to_vec(),
        };
        source.sync(host, &sopts).await?;
        if self.is_cancelled() {
            return Ok(Some(Stop::Cancelled));
        }
        self.drain_queue(host, kinds, &opts.limits).await
    }

    /// Fetch queued items, committing in chunks of `pipeline.commit_batch`.
    pub(crate) async fn drain_queue(
        &self,
        host: &Host<'_>,
        kinds: &[SourceKind],
        limits: &Limits,
    ) -> Result<Option<Stop>, PipelineError> {
        let (rows, batch, jobs) = {
            let cat = self.catalog();
            let rows: Vec<QueueRow> = cat
                .queue(Some(&host.account.id), kinds)?
                .into_iter()
                .filter(|r| r.attempts < MAX_QUEUE_ATTEMPTS)
                .collect();
            let batch: usize = cat.setting_or("pipeline.commit_batch", 20usize)?;
            let jobs = host
                .account
                .config
                .get("fetch_jobs")
                .and_then(Value::as_u64)
                .unwrap_or(4) as usize;
            (rows, batch.max(1), jobs.max(1))
        };
        if rows.is_empty() {
            return Ok(None);
        }
        let total = rows.len() as u64;
        let mut committed = 0u64;
        for chunk in rows.chunks(batch) {
            if self.is_cancelled() {
                return Ok(Some(Stop::Cancelled));
            }
            if limits.time_exceeded() {
                return Ok(Some(Stop::Limit("--time-limit".into())));
            }
            let reqs: Vec<(QueueRow, FetchRequest)> = {
                let cat = self.catalog();
                chunk
                    .iter()
                    .map(|r| {
                        let existing = cat.entry_by_key(
                            host.account.id.as_str(),
                            r.source_kind,
                            &r.source_id,
                        )?;
                        let req = FetchRequest {
                            source_kind: r.source_kind,
                            source_id: r.source_id.clone(),
                            fetch_state: existing.as_ref().and_then(|e| e.fetch_state.clone()),
                            metadata: existing.map(|e| e.metadata).unwrap_or(Value::Null),
                            hint: r.hint.clone(),
                            full: false,
                        };
                        Ok((r.clone(), req))
                    })
                    .collect::<Result<_, PipelineError>>()?
            };
            let results: Vec<(QueueRow, Result<FetchOutcome, SourceError>)> =
                futures::stream::iter(reqs)
                    .map(|(row, req)| async move {
                        let r = host.source.fetch(host, &req).await;
                        (row, r)
                    })
                    .buffered(jobs)
                    .collect()
                    .await;
            let mut items = Vec::new();
            let mut dequeue = Vec::new();
            for (row, res) in results {
                let key = (row.source_kind, row.source_id.clone());
                match res {
                    Ok(FetchOutcome::Fetched(fe)) => items.push(Item {
                        fetched: *fe,
                        dequeue: Some(key),
                    }),
                    Ok(FetchOutcome::Unchanged) => {
                        host.stat(row.source_kind, |s| s.unchanged += 1);
                        dequeue.push(key);
                    }
                    Ok(FetchOutcome::NotApplicable(why)) => {
                        tracing::debug!(source_id = %row.source_id, "not applicable: {why}");
                        host.stat(row.source_kind, |s| s.not_applicable += 1);
                        dequeue.push(key);
                    }
                    Ok(FetchOutcome::NotFound(why)) => {
                        tracing::info!(source_id = %row.source_id, "not found: {why}");
                        host.stat(row.source_kind, |s| s.not_applicable += 1);
                        dequeue.push(key);
                    }
                    Err(e @ (SourceError::Auth(_) | SourceError::Cancelled)) => {
                        // Commit what was fetched before giving up.
                        host.commit_items(items, vec![], vec![], dequeue)?;
                        return Err(e.into());
                    }
                    Err(e) => {
                        host.stat(row.source_kind, |s| s.failed += 1);
                        host.error(format!("{} {}: {e}", row.source_kind, row.source_id));
                        self.catalog().queue_failure(
                            &host.account.id,
                            row.source_kind,
                            &row.source_id,
                            &e.to_string(),
                        )?;
                    }
                }
            }
            committed += items.len() as u64;
            host.commit_items(items, vec![], vec![], dequeue)?;
            self.emit(Progress::Fetched {
                account: host.account.id.to_string(),
                committed,
                queued: total.saturating_sub(committed),
            });
        }
        Ok(None)
    }

    fn sync_dry_run(&self, opts: &SyncOptions) -> Result<SyncReport, PipelineError> {
        let mut dr = DryRun::default();
        for account in self.selected_accounts(opts)? {
            let source = self.source_for(&account)?;
            let (sources, skipped_reason) = match (&source, account.status) {
                (_, AccountStatus::Disabled) => (vec![], Some("disabled".to_string())),
                (_, AccountStatus::NeedsReauth) => {
                    (vec![], Some("needs re-authentication".to_string()))
                }
                (None, _) => (
                    vec![],
                    Some("no sources for this account kind yet".to_string()),
                ),
                (Some(s), _) if !s.supports_sync() => {
                    (vec![], Some("on-demand source only".to_string()))
                }
                (Some(s), _) => (
                    s.kinds()
                        .iter()
                        .filter(|k| opts.sources.is_empty() || opts.sources.contains(k))
                        .map(|k| k.to_string())
                        .collect(),
                    None,
                ),
            };
            let queued = self.catalog().queue(Some(&account.id), &[])?.len() as u64;
            dr.accounts.push(DryRunAccount {
                id: account.id.to_string(),
                kind: account.kind.to_string(),
                status: account.status.to_string(),
                sources,
                queued,
                skipped_reason,
            });
        }
        let cat = self.catalog();
        let policy = SummaryPolicy::load(&cat)?;
        dr.pending_summaries = cat
            .summarization_candidates(&EntryFilter::default(), policy.max_attempts)?
            .len() as u64;
        Ok(SyncReport {
            dry_run: Some(dr),
            ..Default::default()
        })
    }
}

/// Group counts by source kind (for reports).
pub fn by_kind(stats: &RunStats) -> BTreeMap<String, u64> {
    let mut m = BTreeMap::new();
    for (k, s) in &stats.sources {
        let kind = k.split_once('/').map(|(_, k)| k).unwrap_or(k);
        *m.entry(kind.to_string()).or_default() += s.new + s.updated;
    }
    m
}
