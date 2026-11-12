//! `sb scan-links`: the document links in stored Slack entries and their state,
//! and with `--ingest` the backfill of links found before a kind was enabled
//! (docs/specs/link-ingest.md, ADR-0022).

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Utc};
use second_brain_kernel::{
    AccountKind, EntryOrigin, NormalizeOutcome, RawStatus, RunStatus, SourceKind,
};
use second_brain_store::{EntryFilter, StoreError, SyncLock};
use serde::Serialize;

use crate::Pipeline;
use crate::error::PipelineError;
use crate::host::Host;
use crate::link_stage::{LinkStageOptions, LinkStats};
use crate::policy::SummaryPolicy;

/// Options of `sb scan-links`.
#[derive(Debug, Clone, Default)]
pub struct ScanLinksOptions {
    pub accounts: Vec<String>,
    /// Only these kinds; with `ingest` they also replace the enabled kinds.
    pub kinds: Vec<SourceKind>,
    /// Only entries dated at or after this instant.
    pub since: Option<DateTime<Utc>>,
    /// At most this many links are listed or ingested (default `links.max_per_run`).
    pub limit: Option<u64>,
    pub ingest: bool,
    /// With `ingest`: also try `unavailable` and `abandoned` links now.
    pub retry: bool,
}

/// One link of a stored conversation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScannedLink {
    /// May carry a sharing token: printed for the user, never logged.
    pub url: String,
    pub host: String,
    pub source_kind: SourceKind,
    /// `new` (not recorded yet) or a link status.
    pub status: String,
    pub account: String,
    pub from_entry_uid: String,
    pub from_title: String,
    pub message_date: Option<String>,
    pub entry_uid: Option<String>,
    pub last_error: Option<String>,
    pub next_attempt_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ScanLinksReport {
    pub run_id: Option<i64>,
    pub links: Vec<ScannedLink>,
    /// Links per status.
    pub counts: BTreeMap<String, u64>,
    /// Links beyond the limit that are not listed.
    pub not_listed: u64,
    /// Slack entries whose raw data is missing or that failed to normalize.
    pub skipped_entries: u64,
    /// The link stage, with `ingest`.
    pub stage: Option<LinkStats>,
    pub errors: Vec<String>,
}

impl Pipeline {
    /// List the links of stored Slack entries; with `ingest`, record them and run
    /// the link stage on them.
    pub async fn scan_links(&self, o: &ScanLinksOptions) -> Result<ScanLinksReport, PipelineError> {
        let _lock = if o.ingest {
            match SyncLock::try_acquire(self.home(), "scan-links") {
                Ok(l) => Some(l),
                Err(StoreError::Locked(_)) => return Err(PipelineError::Locked),
                Err(e) => return Err(e.into()),
            }
        } else {
            None
        };
        let mut report = ScanLinksReport::default();
        let run_id = if o.ingest {
            let cat = self.catalog();
            cat.close_stale_runs("scan-links", cat.now())?;
            let id = cat.start_run("scan-links", self.trigger)?;
            report.run_id = Some(id);
            self.set_run_id(id);
            Some(id)
        } else {
            None
        };
        let result = self.scan_inner(o, &mut report).await;
        if let Some(id) = run_id {
            let status = match &result {
                Err(_) => RunStatus::Failed,
                Ok(()) if self.is_cancelled() => RunStatus::Interrupted,
                Ok(()) if !report.errors.is_empty() => RunStatus::Partial,
                Ok(()) => RunStatus::Ok,
            };
            let stats = serde_json::to_value(&report)?;
            self.catalog().finish_run(
                id,
                status,
                &stats,
                report.errors.first().map(String::as_str),
            )?;
        }
        result?;
        Ok(report)
    }

    async fn scan_inner(
        &self,
        o: &ScanLinksOptions,
        report: &mut ScanLinksReport,
    ) -> Result<(), PipelineError> {
        let policy = SummaryPolicy::load(&self.catalog())?;
        let limit = match o.limit {
            Some(l) => l,
            None => self.catalog().setting_or("links.max_per_run", 50u64)?,
        };
        for want in &o.accounts {
            let a = self.account(want)?;
            if a.kind != AccountKind::Slack {
                return Err(PipelineError::Invalid(format!(
                    "{want} is not a Slack account"
                )));
            }
        }
        let filter = EntryFilter {
            accounts: o.accounts.clone(),
            source_kinds: vec![SourceKind::SlackThread, SourceKind::SlackDay],
            since: o.since,
            ascending_id: true,
            ..Default::default()
        };
        let entries = self.catalog().list_entries(&filter)?;
        let mut hosts: BTreeMap<String, Host<'_>> = BTreeMap::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut found: Vec<ScannedLink> = Vec::new();
        for e in &entries {
            if self.is_cancelled() {
                break;
            }
            if e.raw_status != RawStatus::Present {
                report.skipped_entries += 1;
                continue;
            }
            if !hosts.contains_key(&e.account_id) {
                let account = self.account(&e.account_id)?;
                let Some(source) = self.source_for(&account)? else {
                    report.skipped_entries += 1;
                    continue;
                };
                hosts.insert(
                    e.account_id.clone(),
                    Host::new(self, account, source, &policy, EntryOrigin::Sync)?,
                );
            }
            let host = &hosts[&e.account_id];
            let links = match host.normalize_stored(e) {
                Ok(NormalizeOutcome::Entry(n)) => n.links,
                Ok(NormalizeOutcome::NotApplicable(_)) => continue,
                Err(err) => {
                    report.skipped_entries += 1;
                    report.errors.push(format!("{}: {err}", e.entry_uid));
                    continue;
                }
            };
            let links: Vec<_> = links
                .into_iter()
                .filter(|l| o.kinds.is_empty() || o.kinds.contains(&l.link.kind))
                .collect();
            if links.is_empty() {
                continue;
            }
            if o.ingest {
                self.catalog().record_links(&e.account_id, e.id, &links)?;
            }
            for l in links {
                if !seen.insert((e.account_id.clone(), l.link.key.clone())) {
                    continue;
                }
                let row = self.catalog().link(&e.account_id, &l.link.key)?;
                // The first reference is the one the table keeps.
                let (from_uid, from_title) = match row.as_ref().and_then(|r| r.first_entry_id) {
                    Some(id) if id != e.id => match self.catalog().entry(id)? {
                        Some(f) => (f.entry_uid, f.title),
                        None => (e.entry_uid.clone(), e.title.clone()),
                    },
                    _ => (e.entry_uid.clone(), e.title.clone()),
                };
                let entry_uid = match row.as_ref().and_then(|r| r.entry_id) {
                    Some(id) => self.catalog().entry(id)?.map(|x| x.entry_uid),
                    None => None,
                };
                found.push(ScannedLink {
                    url: l.link.url.clone(),
                    host: l.link.host.clone(),
                    source_kind: l.link.kind,
                    status: row
                        .as_ref()
                        .map(|r| r.status.to_string())
                        .unwrap_or_else(|| "new".into()),
                    account: e.account_id.clone(),
                    from_entry_uid: from_uid,
                    from_title,
                    message_date: slack_ts(&l.message_ts),
                    entry_uid,
                    last_error: row.as_ref().and_then(|r| r.last_error.clone()),
                    next_attempt_at: row
                        .as_ref()
                        .and_then(|r| r.next_attempt_at)
                        .map(second_brain_kernel::util::ts),
                });
            }
        }
        drop(hosts);

        if o.ingest && !self.is_cancelled() {
            let stage = self
                .run_link_stage(&LinkStageOptions {
                    accounts: o.accounts.clone(),
                    kinds_override: (!o.kinds.is_empty()).then(|| o.kinds.clone()),
                    max: Some(limit),
                    retry: o.retry,
                    ..Default::default()
                })
                .await?;
            report.errors.extend(stage.errors);
            report.stage = Some(stage.stats);
            // Show the state after the stage.
            for l in &mut found {
                let key = second_brain_kernel::links::classify_link(&l.url).map(|c| c.key);
                if let Some(row) =
                    key.and_then(|k| self.catalog().link(&l.account, &k).ok().flatten())
                {
                    l.status = row.status.to_string();
                    l.last_error = row.last_error;
                    l.next_attempt_at = row.next_attempt_at.map(second_brain_kernel::util::ts);
                    l.entry_uid = row
                        .entry_id
                        .and_then(|id| self.catalog().entry(id).ok().flatten())
                        .map(|e| e.entry_uid);
                }
            }
        }

        for l in &found {
            *report.counts.entry(l.status.clone()).or_default() += 1;
        }
        found.sort_by(|a, b| b.message_date.cmp(&a.message_date).then(a.url.cmp(&b.url)));
        report.not_listed = found.len().saturating_sub(limit as usize) as u64;
        found.truncate(limit as usize);
        report.links = found;
        Ok(())
    }
}

/// `1727740800.000100` as an RFC 3339 time.
fn slack_ts(ts: &str) -> Option<String> {
    let secs: i64 = ts.split('.').next()?.parse().ok()?;
    DateTime::from_timestamp(secs, 0).map(second_brain_kernel::util::ts)
}
