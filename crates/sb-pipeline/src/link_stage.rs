//! The link stage of `sb sync`: ingest of the document links found in Slack
//! entries, for the source kinds the user enabled (docs/specs/link-ingest.md,
//! ADR-0022).

use std::collections::HashMap;

use futures::StreamExt;
use second_brain_kernel::document::IngestHint;
use second_brain_kernel::{AccountKind, AccountStatus, EntryOrigin, SourceKind};
use second_brain_store::{Account, LinkOutcome, LinkRow};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::Pipeline;
use crate::error::PipelineError;
use crate::host::Host;
use crate::ingest::{
    Fail, FailKind, IngestOptions, IngestResult, IngestStatus, Locator, Phase, Work, classify,
};
use crate::policy::SummaryPolicy;
use crate::run::{Limits, Stop};

/// Links fetched at the same time.
const CONCURRENCY: usize = 4;

/// Options of the link stage.
#[derive(Debug, Clone, Default)]
pub struct LinkStageOptions {
    /// Only these Slack accounts (empty = all).
    pub accounts: Vec<String>,
    /// Replaces the enabled kinds (`sb scan-links --kind`).
    pub kinds_override: Option<Vec<SourceKind>>,
    /// Replaces `links.max_per_run`.
    pub max: Option<u64>,
    pub limits: Limits,
    /// Make `unavailable` and `abandoned` links due again first.
    pub retry: bool,
}

/// What the stage did in one run.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct LinkStats {
    /// Links due when the stage started and allowed by the settings.
    pub due: u64,
    pub ingested: u64,
    pub duplicate: u64,
    pub not_applicable: u64,
    pub unavailable: u64,
    pub abandoned: u64,
    /// Left for the next run: no usable account, a network error, an interruption.
    pub waiting: u64,
    /// Due, but beyond `links.max_per_run` or a limit.
    pub deferred_by_limit: u64,
}

impl LinkStats {
    pub fn is_empty(&self) -> bool {
        *self == LinkStats::default()
    }
}

/// The result of the stage: counters, errors and why it stopped.
#[derive(Debug, Clone, Default)]
pub struct LinkStageReport {
    pub stats: LinkStats,
    pub errors: Vec<String>,
    pub stop: Option<Stop>,
    /// Entries created or updated, per `<account>/<source_kind>`.
    pub entries: HashMap<String, (u64, u64)>,
}

/// The source kinds whose links a Slack account ingests: its own `ingest_links`
/// when present, else `links.ingest_kinds`. Unknown names are ignored.
pub fn effective_kinds(global: &Value, account: &Account) -> Vec<SourceKind> {
    let list = account.config.get("ingest_links").unwrap_or(global);
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|s| s.parse::<SourceKind>().ok())
        .filter(|k| matches!(k, SourceKind::GoogleDoc | SourceKind::MicrosoftFile))
        .collect()
}

/// Whether a host passes the account's `ingest_link_hosts` (empty = any). `*`
/// in a pattern matches any run of characters; matching ignores case.
pub fn host_allowed(account: &Account, host: &str) -> bool {
    let patterns: Vec<&str> = account
        .config
        .get("ingest_link_hosts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    patterns.is_empty() || patterns.iter().any(|p| glob_match(p, host))
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

impl Pipeline {
    /// Run the link stage. The caller holds the sync lock.
    pub async fn run_link_stage(
        &self,
        opts: &LinkStageOptions,
    ) -> Result<LinkStageReport, PipelineError> {
        let mut report = LinkStageReport::default();
        let (policy, global, max_per_run, max_attempts, slack) = {
            let cat = self.catalog();
            let policy = SummaryPolicy::load(&cat)?;
            let global = cat
                .setting("links.ingest_kinds")?
                .unwrap_or_else(|| json!([]));
            let max_per_run = opts
                .max
                .unwrap_or(cat.setting_or("links.max_per_run", 50u64)?);
            let max_attempts = cat.setting_or("links.max_attempts", 6i64)?;
            let slack: Vec<Account> = cat
                .accounts()?
                .into_iter()
                .filter(|a| a.kind == AccountKind::Slack && a.status != AccountStatus::Disabled)
                .filter(|a| {
                    opts.accounts.is_empty() || opts.accounts.iter().any(|w| w == a.id.as_str())
                })
                .collect();
            (policy, global, max_per_run, max_attempts, slack)
        };
        // Select the due links of every workspace first, newest first.
        let mut selected: Vec<(Account, LinkRow)> = Vec::new();
        let mut beyond = 0u64;
        let now = self.catalog().now();
        for account in &slack {
            let kinds = match &opts.kinds_override {
                Some(k) => k.clone(),
                None => effective_kinds(&global, account),
            };
            if kinds.is_empty() {
                continue;
            }
            if opts.retry {
                self.catalog()
                    .retry_links(Some(account.id.as_str()), &kinds)?;
            }
            let due = self
                .catalog()
                .due_links(account.id.as_str(), &kinds, now, u64::MAX)?;
            for l in due {
                if !host_allowed(account, &l.host) {
                    continue;
                }
                if (selected.len() as u64) < max_per_run {
                    selected.push((account.clone(), l));
                } else {
                    beyond += 1;
                }
            }
        }
        report.stats.due = selected.len() as u64 + beyond;
        report.stats.deferred_by_limit = beyond;
        if selected.is_empty() {
            return Ok(report);
        }

        let hosts = self.ingest_hosts(&policy, EntryOrigin::Link)?;
        let mut stream = futures::stream::iter(selected)
            .map(|(account, link)| {
                let hosts = &hosts;
                async move {
                    if self.is_cancelled() {
                        return (account, link, Err(Fail::interrupted()));
                    }
                    let res = self.fetch_link(&link, hosts).await;
                    (account, link, res)
                }
            })
            .buffer_unordered(CONCURRENCY);
        let mut left_over = 0u64;
        while let Some((account, link, res)) = stream.next().await {
            if report.stop.is_some() || self.is_cancelled() || opts.limits.time_exceeded() {
                report.stop.get_or_insert(if self.is_cancelled() {
                    Stop::Cancelled
                } else {
                    Stop::Limit("--time-limit".into())
                });
                left_over += 1;
                continue;
            }
            let outcome = self.link_outcome(&hosts, res, max_attempts);
            let outcome = match outcome {
                Ok(o) => o,
                Err(e) => {
                    report.errors.push(format!("link ({}): {e}", link.host));
                    LinkOutcome::Waiting {
                        error: e.to_string(),
                    }
                }
            };
            self.count_outcome(&mut report.stats, &link, &outcome, max_attempts);
            self.catalog()
                .record_link_outcome(account.id.as_str(), &link.link_key, &outcome)?;
        }
        drop(stream);
        report.stats.deferred_by_limit += left_over;
        for h in hosts.values() {
            if let Ok(s) = h.stats.lock() {
                for (kind, st) in s.iter() {
                    let e = report
                        .entries
                        .entry(format!("{}/{}", h.account.id, kind))
                        .or_default();
                    e.0 += st.new;
                    e.1 += st.updated;
                }
            }
            if let Ok(e) = h.errors.lock() {
                report.errors.extend(e.iter().cloned());
            }
        }
        Ok(report)
    }

    /// Fetch one link on the ingest path (no commit).
    async fn fetch_link(
        &self,
        link: &LinkRow,
        hosts: &HashMap<String, Host<'_>>,
    ) -> Result<Phase, Fail> {
        let locator = classify(&link.url).map_err(Fail::other)?;
        if matches!(locator, Locator::Web(_) | Locator::Local(_)) {
            return Err(Fail::other("not a Drive or SharePoint file link"));
        }
        let mut extra = Map::new();
        if let Some(excerpt) = link.excerpt.as_deref().filter(|e| !e.trim().is_empty()) {
            extra.insert(
                second_brain_kernel::document::keys::LINK_CONTEXT.into(),
                json!(excerpt),
            );
        }
        if let Some(id) = link.first_entry_id
            && let Ok(Some(e)) = self.catalog().entry(id)
        {
            extra.insert(
                "discovered_via".into(),
                json!({"entry_uid": e.entry_uid, "title": e.title, "url": e.source_url}),
            );
        }
        let hint = IngestHint {
            locator: link.url.clone(),
            extra,
            ..Default::default()
        };
        let work = Work {
            idx: 0,
            raw: link.url.clone(),
            locator,
        };
        self.fetch_inner(&work, hosts, &IngestOptions::default(), &hint)
            .await
    }

    /// Commit a fetched link and turn the result into what the table records.
    fn link_outcome(
        &self,
        hosts: &HashMap<String, Host<'_>>,
        res: Result<Phase, Fail>,
        max_attempts: i64,
    ) -> Result<LinkOutcome, PipelineError> {
        let entry_id = |r: &IngestResult| -> Option<i64> {
            let uid = r.entry_uid.as_deref()?;
            self.catalog()
                .entry_by_uid(uid)
                .ok()
                .flatten()
                .map(|e| e.id)
        };
        let from_result = |r: IngestResult| -> LinkOutcome {
            match r.status {
                IngestStatus::Created | IngestStatus::Updated | IngestStatus::Unchanged => {
                    match entry_id(&r) {
                        Some(entry_id) => LinkOutcome::Ingested { entry_id },
                        None => LinkOutcome::Waiting {
                            error: "the entry was not stored".into(),
                        },
                    }
                }
                IngestStatus::Duplicate => {
                    let dup = r
                        .duplicate_of
                        .as_deref()
                        .and_then(|u| self.catalog().entry_by_uid(u).ok().flatten())
                        .map(|e| e.id);
                    match dup.or_else(|| entry_id(&r)) {
                        Some(entry_id) => LinkOutcome::Duplicate { entry_id },
                        None => LinkOutcome::NotApplicable {
                            reason: r.message.unwrap_or_else(|| "duplicate".into()),
                        },
                    }
                }
                IngestStatus::NotApplicable => LinkOutcome::NotApplicable {
                    reason: r.message.unwrap_or_else(|| "not applicable".into()),
                },
                _ => LinkOutcome::Waiting {
                    error: r.message.unwrap_or_else(|| "failed".into()),
                },
            }
        };
        Ok(match res {
            Ok(Phase::Done(_, r)) => from_result(r),
            Ok(Phase::Commit {
                raw,
                item,
                account,
                existed,
                ..
            }) => from_result(self.commit_one(hosts, &account, *item, existed, &raw)?),
            Ok(Phase::Overrides { entry, .. }) => LinkOutcome::Ingested { entry_id: entry.id },
            Err(f) => match f.kind {
                FailKind::NoAccess | FailKind::Other => LinkOutcome::Unavailable {
                    error: f.msg,
                    max_attempts,
                },
                FailKind::NoAccount | FailKind::Transient | FailKind::Interrupted => {
                    LinkOutcome::Waiting { error: f.msg }
                }
            },
        })
    }

    fn count_outcome(
        &self,
        stats: &mut LinkStats,
        link: &LinkRow,
        outcome: &LinkOutcome,
        max_attempts: i64,
    ) {
        match outcome {
            LinkOutcome::Ingested { .. } => stats.ingested += 1,
            LinkOutcome::Duplicate { .. } => stats.duplicate += 1,
            LinkOutcome::NotApplicable { .. } => stats.not_applicable += 1,
            LinkOutcome::Waiting { .. } => stats.waiting += 1,
            LinkOutcome::Unavailable { .. } => {
                if link.attempts + 1 >= max_attempts {
                    stats.abandoned += 1;
                } else {
                    stats.unavailable += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_patterns() {
        assert!(glob_match(
            "contoso.sharepoint.com",
            "Contoso.SharePoint.com"
        ));
        assert!(glob_match("*.sharepoint.com", "contoso-my.sharepoint.com"));
        assert!(glob_match("*", "anything"));
        assert!(!glob_match("*.sharepoint.com", "sharepoint.com"));
        assert!(!glob_match(
            "contoso.sharepoint.com",
            "other.sharepoint.com"
        ));
        assert!(glob_match("docs.*.com", "docs.google.com"));
    }
}
