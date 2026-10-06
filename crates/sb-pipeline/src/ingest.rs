//! `sb ingest`: single-item ingest of Google Docs, web pages and local files
//! (docs/specs/ingest.md, ADR-0014).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use regex::Regex;
use sb_core::document::IngestHint;
use sb_core::source::{Source, SourceError};
use sb_core::{
    AccountKind, AccountStatus, EntryOrigin, FetchOutcome, FetchRequest, FetchedEntry, RawRole,
    RawStatus, RunStatus, Severity, SourceKind,
};
use sb_store::{Account, Entry, EntryFilter, StoreError, SyncLock};
use serde::Serialize;
use serde_json::Value;

use crate::Pipeline;
use crate::error::PipelineError;
use crate::host::{Host, Item, merge_json};
use crate::policy::SummaryPolicy;
use crate::run::{Limits, Stop};
use crate::summarize::{SummarizeOptions, SummarizeReport, Target};

/// Locators fetched at the same time.
const FETCH_CONCURRENCY: usize = 4;
/// Locators accepted by one call.
pub const MAX_LOCATORS: usize = 50;

/// Options of `sb ingest`.
#[derive(Debug, Clone, Default)]
pub struct IngestOptions {
    pub account: Option<String>,
    pub title: Option<String>,
    pub context: Option<String>,
    pub date: Option<DateTime<Utc>>,
    pub force: bool,
    pub keep_original: bool,
    pub no_summary: bool,
    pub dry_run: bool,
}

/// What a locator turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Locator {
    /// A Google Docs, Sheets, Slides or Drive file, by Drive file ID.
    Google(String),
    /// An `http(s)` URL.
    Web(String),
    /// A file path (as given; the source canonicalizes it).
    Local(PathBuf),
}

impl Locator {
    pub fn source_kind(&self) -> SourceKind {
        match self {
            Locator::Google(_) => SourceKind::GoogleDoc,
            Locator::Web(_) => SourceKind::WebPage,
            Locator::Local(_) => SourceKind::LocalFile,
        }
    }
}

/// Classify a locator (docs/specs/ingest.md "Locator classification"). The
/// error is a usage message.
pub fn classify(locator: &str) -> Result<Locator, String> {
    let s = locator.trim();
    if s.is_empty() {
        return Err("empty locator".into());
    }
    let parsed = url::Url::parse(s);
    let Ok(u) = parsed else {
        return Ok(Locator::Local(PathBuf::from(s)));
    };
    match u.scheme() {
        // A Windows drive path such as `C:\notes\a.md` parses as scheme `c`.
        sch if sch.len() == 1 => Ok(Locator::Local(PathBuf::from(s))),
        "file" => u
            .to_file_path()
            .map(Locator::Local)
            .map_err(|_| format!("not a local file URL: {s}")),
        "http" | "https" => {
            let host = u.host_str().unwrap_or("").to_ascii_lowercase();
            if host == "docs.google.com" || host == "drive.google.com" {
                if u.path().contains("/folders/") {
                    return Err("Drive folders are not supported; ingest the files in it".into());
                }
                if !u.path().contains("/d/e/")
                    && let Some(id) = google_file_id(&u)
                {
                    return Ok(Locator::Google(id));
                }
            }
            if host.ends_with(".slack.com") && u.path().starts_with("/archives") {
                return Err("Slack content is ingested by `sb sync`".into());
            }
            Ok(Locator::Web(s.to_string()))
        }
        other => Err(format!("unsupported scheme `{other}:`")),
    }
}

fn google_file_id(u: &url::Url) -> Option<String> {
    let re =
        Regex::new(r"/(?:document|spreadsheets|presentation|file)/(?:u/\d+/)?d/([A-Za-z0-9_-]+)")
            .ok()?;
    if let Some(c) = re.captures(u.path()) {
        return Some(c[1].to_string());
    }
    if matches!(u.path(), "/open" | "/uc") {
        return u
            .query_pairs()
            .find(|(k, _)| k == "id")
            .map(|(_, v)| v.into_owned());
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestStatus {
    Created,
    Updated,
    Unchanged,
    Duplicate,
    NotApplicable,
    Failed,
    WouldCreate,
    WouldUpdate,
}

/// The outcome for one locator.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IngestResult {
    pub locator: String,
    pub status: IngestStatus,
    pub entry_uid: Option<String>,
    pub source_kind: Option<String>,
    pub account: Option<String>,
    pub title: Option<String>,
    pub source_url: Option<String>,
    pub summary_status: Option<String>,
    pub duplicate_of: Option<String>,
    pub message: Option<String>,
}

impl IngestResult {
    fn new(locator: &str, status: IngestStatus, message: Option<String>) -> Self {
        IngestResult {
            locator: locator.to_string(),
            status,
            entry_uid: None,
            source_kind: None,
            account: None,
            title: None,
            source_url: None,
            summary_status: None,
            duplicate_of: None,
            message,
        }
    }

    fn failed(locator: &str, why: impl Into<String>) -> Self {
        IngestResult::new(locator, IngestStatus::Failed, Some(why.into()))
    }

    fn with_entry(mut self, e: &Entry) -> Self {
        self.entry_uid = Some(e.entry_uid.clone());
        self.source_kind = Some(e.source_kind.to_string());
        self.account = Some(e.account_id.clone());
        self.title = Some(e.title.clone());
        self.source_url = e.source_url.clone();
        self.summary_status = Some(e.summary_status.to_string());
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct IngestReport {
    pub run_id: Option<i64>,
    pub results: Vec<IngestResult>,
    /// Locators that were classified successfully.
    pub valid: u64,
    pub stop: Option<Stop>,
    pub summarize: Option<SummarizeReport>,
    /// Entries of this call whose summary is still `pending`.
    pub pending: u64,
}

impl IngestReport {
    /// `true` when at least one locator did not end well.
    pub fn has_problems(&self) -> bool {
        self.results
            .iter()
            .any(|r| matches!(r.status, IngestStatus::Failed | IngestStatus::NotApplicable))
    }

    fn run_status(&self) -> RunStatus {
        match &self.stop {
            Some(Stop::Cancelled) => RunStatus::Interrupted,
            Some(Stop::Limit(_)) => RunStatus::StoppedByLimit,
            None if self.has_problems() => RunStatus::Partial,
            None => RunStatus::Ok,
        }
    }
}

/// A locator to work on, with its position in the call.
struct Work {
    idx: usize,
    raw: String,
    locator: Locator,
}

/// The result of the fetch phase for one locator.
enum Phase {
    /// Nothing to commit.
    Done(usize, IngestResult),
    /// A fetched entry to commit.
    Commit {
        idx: usize,
        raw: String,
        item: Box<FetchedEntry>,
        account: String,
        existed: bool,
    },
    /// An unchanged entry whose title, context or date changed.
    Overrides {
        idx: usize,
        raw: String,
        entry: Box<Entry>,
        account: String,
    },
}

impl Pipeline {
    /// Ingest locators. See docs/specs/ingest.md.
    pub async fn ingest(
        &self,
        locators: &[String],
        opts: &IngestOptions,
    ) -> Result<IngestReport, PipelineError> {
        if locators.is_empty() {
            return Err(PipelineError::Invalid("no locator given".into()));
        }
        if locators.len() > MAX_LOCATORS {
            return Err(PipelineError::Invalid(format!(
                "too many locators ({}; at most {MAX_LOCATORS} per call)",
                locators.len()
            )));
        }
        if locators.len() > 1 && (opts.title.is_some() || opts.date.is_some()) {
            return Err(PipelineError::Invalid(
                "--title and --date can only be used with one locator".into(),
            ));
        }
        if !opts.dry_run {
            self.upgrade_input_hashes()?;
        }
        let mut report = IngestReport::default();
        let mut slots: Vec<Option<IngestResult>> = vec![None; locators.len()];
        let mut work = Vec::new();
        for (idx, raw) in locators.iter().enumerate() {
            match classify(raw) {
                Ok(locator) => {
                    report.valid += 1;
                    work.push(Work {
                        idx,
                        raw: raw.clone(),
                        locator,
                    });
                }
                Err(why) => slots[idx] = Some(IngestResult::failed(raw, why)),
            }
        }
        if report.valid == 0 {
            report.results = slots.into_iter().flatten().collect();
            return Ok(report);
        }
        if let Some(a) = &opts.account {
            let acc = self.account(a)?;
            if acc.kind != AccountKind::Google {
                return Err(PipelineError::Invalid(format!(
                    "{a} is not a Google account"
                )));
            }
        }
        let hint = IngestHint {
            locator: String::new(),
            context: opts.context.clone(),
            title: opts.title.clone(),
            date: opts.date,
            keep_original: opts.keep_original,
        };
        if opts.dry_run {
            self.ingest_dry_run(&work, opts, &mut slots)?;
            report.results = slots.into_iter().flatten().collect();
            return Ok(report);
        }

        let _lock = match SyncLock::try_acquire(self.home(), "ingest") {
            Ok(l) => l,
            Err(StoreError::Locked(_)) => return Err(PipelineError::Locked),
            Err(e) => return Err(e.into()),
        };
        let run_id = {
            let cat = self.catalog();
            cat.close_stale_runs("ingest", cat.now())?;
            cat.start_run("ingest", self.trigger)?
        };
        self.set_run_id(run_id);
        report.run_id = Some(run_id);

        let policy = SummaryPolicy::load(&self.catalog())?;
        let hosts = self.ingest_hosts(&policy)?;
        let mut committed: Vec<String> = Vec::new();

        let mut stream = futures::stream::iter(work)
            .map(|w| self.fetch_phase(w, &hosts, opts, &hint))
            .buffer_unordered(FETCH_CONCURRENCY);
        while let Some(phase) = stream.next().await {
            match phase {
                Phase::Done(idx, r) => slots[idx] = Some(r),
                Phase::Commit {
                    idx,
                    raw,
                    item,
                    account,
                    existed,
                } => {
                    // Checked here, one at a time, so that two copies of a file in one
                    // call see each other.
                    if item.source_ref.source_kind == SourceKind::LocalFile
                        && !existed
                        && !opts.force
                        && let Some(dup) = self.local_duplicate(&item)
                    {
                        let mut r = IngestResult::new(
                            &raw,
                            IngestStatus::Duplicate,
                            Some(format!(
                                "the same content is already ingested from {} ({})",
                                dup.source_url.clone().unwrap_or_default(),
                                dup.entry_uid
                            )),
                        )
                        .with_entry(&dup);
                        r.duplicate_of = Some(dup.entry_uid.clone());
                        slots[idx] = Some(r);
                        continue;
                    }
                    let r = self.commit_one(&hosts, &account, *item, existed, &raw)?;
                    if let Some(uid) = &r.entry_uid {
                        committed.push(uid.clone());
                    }
                    slots[idx] = Some(r);
                }
                Phase::Overrides {
                    idx,
                    raw,
                    entry,
                    account,
                } => {
                    let r = self.apply_overrides(&hosts, &account, *entry, &hint, &raw)?;
                    if let Some(uid) = &r.entry_uid {
                        committed.push(uid.clone());
                    }
                    slots[idx] = Some(r);
                }
            }
        }
        drop(stream);
        report.results = slots.into_iter().flatten().collect();
        if self.is_cancelled() {
            report.stop = Some(Stop::Cancelled);
        }

        if !opts.no_summary && report.stop.is_none() && !committed.is_empty() {
            let s = self
                .summarize_pending(&SummarizeOptions {
                    filter: EntryFilter {
                        entry_uids: committed.clone(),
                        ..Default::default()
                    },
                    target: Target::Configured,
                    limits: Limits::default(),
                    retry_failed: false,
                    force: false,
                    estimate_only: false,
                })
                .await?;
            if let Some(stop) = &s.stop {
                report.stop = Some(stop.clone());
            }
            report.summarize = Some(s);
        }
        // Refresh the entries' state after summarization.
        {
            let cat = self.catalog();
            for r in &mut report.results {
                if let Some(uid) = &r.entry_uid
                    && let Some(e) = cat.entry_by_uid(uid)?
                {
                    r.summary_status = Some(e.summary_status.to_string());
                    r.title = Some(e.title.clone());
                    if e.summary_status == sb_core::SummaryStatus::Skipped
                        && matches!(r.status, IngestStatus::Created | IngestStatus::Updated)
                        && r.message.is_none()
                    {
                        r.message = Some(format!(
                            "shorter than summary.min_chars ({}): kept as searchable text, not summarized",
                            policy.min_chars
                        ));
                    }
                }
            }
        }
        report.pending = report
            .results
            .iter()
            .filter(|r| r.summary_status.as_deref() == Some("pending"))
            .count() as u64;
        let stats = serde_json::to_value(&report)?;
        let err = report
            .results
            .iter()
            .find(|r| r.status == IngestStatus::Failed)
            .and_then(|r| r.message.clone());
        self.catalog()
            .finish_run(run_id, report.run_status(), &stats, err.as_deref())?;
        Ok(report)
    }

    /// A host per account that can take part: the `web` and `local`
    /// pseudo-accounts and the usable Google accounts.
    fn ingest_hosts<'a>(
        &'a self,
        policy: &'a SummaryPolicy,
    ) -> Result<HashMap<String, Host<'a>>, PipelineError> {
        let accounts = self.catalog().accounts()?;
        let mut hosts = HashMap::new();
        for a in accounts {
            let usable = match a.kind {
                AccountKind::Web | AccountKind::Local => a.status != AccountStatus::Disabled,
                AccountKind::Google => a.status == AccountStatus::Active && has_drive(&a),
                _ => false,
            };
            if !usable {
                continue;
            }
            let Some(source) = self.source_for(&a)? else {
                continue;
            };
            let id = a.id.to_string();
            hosts.insert(id, Host::new(self, a, source, policy, EntryOrigin::Ingest)?);
        }
        Ok(hosts)
    }

    async fn fetch_phase(
        &self,
        w: Work,
        hosts: &HashMap<String, Host<'_>>,
        opts: &IngestOptions,
        hint: &IngestHint,
    ) -> Phase {
        if self.is_cancelled() {
            return Phase::Done(w.idx, IngestResult::failed(&w.raw, "interrupted"));
        }
        let mut hint = hint.clone();
        hint.locator = w.raw.clone();
        match self.fetch_inner(&w, hosts, opts, &hint).await {
            Ok(p) => p,
            Err(why) => Phase::Done(w.idx, IngestResult::failed(&w.raw, why)),
        }
    }

    /// Candidate accounts for a locator, best first.
    fn candidates(
        &self,
        w: &Work,
        hosts: &HashMap<String, Host<'_>>,
        opts: &IngestOptions,
    ) -> Result<Vec<String>, String> {
        match &w.locator {
            Locator::Web(_) => Ok(vec!["web".into()]),
            Locator::Local(_) => Ok(vec!["local".into()]),
            Locator::Google(id) => {
                if let Some(a) = &opts.account {
                    return Ok(vec![a.clone()]);
                }
                let mut out: Vec<String> = Vec::new();
                if let Ok(existing) = self
                    .catalog()
                    .entries_by_source_id(SourceKind::GoogleDoc, id)
                {
                    out.extend(existing.into_iter().map(|e| e.account_id));
                }
                let mut rest: Vec<(&String, DateTime<Utc>)> = hosts
                    .iter()
                    .filter(|(_, h)| h.account.kind == AccountKind::Google)
                    .map(|(k, h)| (k, h.account.created_at))
                    .collect();
                rest.sort_by_key(|(k, t)| (*t, (*k).clone()));
                for (k, _) in rest {
                    if !out.contains(k) {
                        out.push(k.clone());
                    }
                }
                if out.is_empty() {
                    return Err(
                        "no usable Google account (add one with `sb account add google`)".into(),
                    );
                }
                Ok(out)
            }
        }
    }

    async fn fetch_inner(
        &self,
        w: &Work,
        hosts: &HashMap<String, Host<'_>>,
        opts: &IngestOptions,
        hint: &IngestHint,
    ) -> Result<Phase, String> {
        let kind = w.locator.source_kind();
        // The same Drive file as a Meet entry is a duplicate.
        if let Locator::Google(id) = &w.locator {
            let meet = self
                .catalog()
                .entries_by_source_id(SourceKind::GoogleMeet, id)
                .map_err(|e| e.to_string())?;
            if let Some(e) = meet.first() {
                let mut r = IngestResult::new(
                    &w.raw,
                    IngestStatus::Duplicate,
                    Some(format!(
                        "already ingested as a google.meet entry ({})",
                        e.entry_uid
                    )),
                )
                .with_entry(e);
                r.duplicate_of = Some(e.entry_uid.clone());
                return Ok(Phase::Done(w.idx, r));
            }
        }
        let candidates = self.candidates(w, hosts, opts)?;
        let mut notes: Vec<String> = Vec::new();
        for account in candidates {
            let Some(host) = hosts.get(&account) else {
                notes.push(format!(
                    "{account}: not usable (disabled, needs re-authentication or no Drive access)"
                ));
                continue;
            };
            let source: Arc<dyn Source> = host.source.clone();
            let Some(sref) = source
                .resolve(&w.raw)
                .or_else(|| source.resolve(&locator_text(&w.locator)))
            else {
                return Err(match w.locator {
                    Locator::Local(_) => "file not found or not readable".to_string(),
                    Locator::Web(_) => "not a valid web address".to_string(),
                    Locator::Google(_) => format!("{account}: cannot resolve the locator"),
                });
            };
            if sref.source_kind != kind {
                notes.push(format!("{account}: wrong source kind"));
                continue;
            }
            let existing = self
                .catalog()
                .entry_by_key(sref.account_id.as_str(), kind, &sref.source_id)
                .map_err(|e| e.to_string())?;
            let (state, full) = self.fetch_state_for(existing.as_ref(), opts)?;
            let req = FetchRequest {
                source_kind: kind,
                source_id: sref.source_id.clone(),
                fetch_state: state,
                metadata: existing
                    .as_ref()
                    .map(|e| e.metadata.clone())
                    .unwrap_or(Value::Null),
                hint: serde_json::to_value(hint).unwrap_or(Value::Null),
                full,
            };
            match source.fetch(host, &req).await {
                Ok(FetchOutcome::Fetched(mut fe)) => {
                    // The key the source chose (a shortcut resolves to its target).
                    if fe.source_ref.account_id.as_str() != account {
                        fe.source_ref.account_id = host.account.id.clone();
                    }
                    return Ok(Phase::Commit {
                        idx: w.idx,
                        raw: w.raw.clone(),
                        existed: existing.is_some(),
                        account,
                        item: fe,
                    });
                }
                Ok(FetchOutcome::Unchanged) => {
                    let Some(e) = existing else {
                        return Err("the source reported no change for a new item".into());
                    };
                    if overrides_differ(&e, hint) {
                        return Ok(Phase::Overrides {
                            idx: w.idx,
                            raw: w.raw.clone(),
                            entry: Box::new(e),
                            account,
                        });
                    }
                    return Ok(Phase::Done(
                        w.idx,
                        IngestResult::new(&w.raw, IngestStatus::Unchanged, None).with_entry(&e),
                    ));
                }
                Ok(FetchOutcome::NotApplicable(why)) => {
                    return Ok(Phase::Done(
                        w.idx,
                        IngestResult::new(&w.raw, IngestStatus::NotApplicable, Some(why)),
                    ));
                }
                Ok(FetchOutcome::NotFound(why)) => {
                    if matches!(w.locator, Locator::Google(_)) {
                        notes.push(format!("{account}: {why}"));
                    } else {
                        return Err(why);
                    }
                }
                Err(e @ SourceError::Auth(_)) => {
                    let msg = format!("{e}; run `sb auth login {account}`");
                    let cat = self.catalog();
                    let _ = cat.set_account_status(&host.account.id, AccountStatus::NeedsReauth);
                    let _ =
                        cat.open_issue(e.issue_code(), Severity::Error, Some(&account), None, &msg);
                    notes.push(format!("{account}: {msg}"));
                }
                Err(SourceError::Cancelled) => return Err("interrupted".into()),
                Err(e) if matches!(w.locator, Locator::Google(_)) => {
                    return Err(format!("{account}: {e}"));
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        Err(notes.join("; "))
    }

    /// The stored fetch state to send, and whether the fetch must be full.
    fn fetch_state_for(
        &self,
        existing: Option<&Entry>,
        opts: &IngestOptions,
    ) -> Result<(Option<Value>, bool), String> {
        let Some(e) = existing else {
            return Ok((None, true));
        };
        if opts.force || e.raw_status != RawStatus::Present {
            return Ok((None, true));
        }
        if opts.keep_original {
            let rows = self
                .catalog()
                .raw_objects(e.id)
                .map_err(|e| e.to_string())?;
            if !rows.iter().any(|r| r.role == RawRole::Primary) {
                return Ok((None, true));
            }
        }
        Ok((e.fetch_state.clone(), false))
    }

    fn local_duplicate(&self, fe: &FetchedEntry) -> Option<Entry> {
        let sha = fe
            .bundle
            .metadata
            .get(sb_core::document::keys::ORIGINAL_SHA256)?
            .as_str()?
            .to_string();
        let found = self
            .catalog()
            .entries_by_metadata_text(
                SourceKind::LocalFile,
                sb_core::document::keys::ORIGINAL_SHA256,
                &sha,
            )
            .ok()?;
        found
            .into_iter()
            .find(|e| e.source_id != fe.source_ref.source_id)
    }

    fn commit_one(
        &self,
        hosts: &HashMap<String, Host<'_>>,
        account: &str,
        fe: FetchedEntry,
        existed: bool,
        raw: &str,
    ) -> Result<IngestResult, PipelineError> {
        let host = hosts
            .get(account)
            .ok_or_else(|| PipelineError::Invalid(format!("unknown account {account}")))?;
        let kind = fe.source_ref.source_kind;
        let key = (
            fe.source_ref.account_id.to_string(),
            fe.source_ref.source_id.clone(),
        );
        let errors_before = host.errors.lock().map(|e| e.len()).unwrap_or(0);
        let counts = |h: &Host<'_>| {
            h.stats
                .lock()
                .ok()
                .and_then(|m| m.get(&kind).cloned())
                .unwrap_or_default()
        };
        let before = counts(host);
        host.commit_items(
            vec![Item {
                fetched: fe,
                dequeue: None,
            }],
            vec![],
            vec![],
            vec![],
        )?;
        let entry = self.catalog().entry_by_key(&key.0, kind, &key.1)?;
        let new_error = host
            .errors
            .lock()
            .ok()
            .and_then(|e| e.get(errors_before).cloned());
        let after = counts(host);
        // What the commit really did, not what was expected: a document that no
        // longer yields text is skipped, and a shortcut's target may already exist.
        let skipped = after.not_applicable > before.not_applicable;
        let status = if after.new > before.new {
            IngestStatus::Created
        } else if after.updated > before.updated || existed {
            IngestStatus::Updated
        } else {
            IngestStatus::Created
        };
        Ok(match (entry, new_error) {
            (_, Some(e)) => IngestResult::failed(raw, e),
            (_, None) if skipped => IngestResult::new(
                raw,
                IngestStatus::NotApplicable,
                Some("no extractable text".into()),
            ),
            (Some(e), None) => IngestResult::new(raw, status, None).with_entry(&e),
            (None, None) => IngestResult::new(
                raw,
                IngestStatus::NotApplicable,
                Some("no extractable text".into()),
            ),
        })
    }

    /// Re-normalize an unchanged entry with a new title, context or date, from
    /// the stored text (no network).
    fn apply_overrides(
        &self,
        hosts: &HashMap<String, Host<'_>>,
        account: &str,
        entry: Entry,
        hint: &IngestHint,
        raw: &str,
    ) -> Result<IngestResult, PipelineError> {
        let host = hosts
            .get(account)
            .ok_or_else(|| PipelineError::Invalid(format!("unknown account {account}")))?;
        let mut e2 = entry.clone();
        e2.metadata = merge_json(&entry.metadata, &Value::Object(hint.metadata()));
        let outcome = host.normalize_stored(&e2)?;
        if let Some(update) = host.renormalized_update(&e2, outcome)? {
            self.catalog().upsert_entry(&update)?;
        }
        let e = self
            .catalog()
            .entry(entry.id)?
            .ok_or_else(|| PipelineError::Invalid("entry vanished".into()))?;
        Ok(IngestResult::new(raw, IngestStatus::Updated, None).with_entry(&e))
    }

    fn ingest_dry_run(
        &self,
        work: &[Work],
        opts: &IngestOptions,
        slots: &mut [Option<IngestResult>],
    ) -> Result<(), PipelineError> {
        let cat_accounts = self.catalog().accounts()?;
        for w in work {
            let kind = w.locator.source_kind();
            let res = match &w.locator {
                Locator::Google(id) => {
                    let cat = self.catalog();
                    let meet = cat.entries_by_source_id(SourceKind::GoogleMeet, id)?;
                    let docs = cat.entries_by_source_id(SourceKind::GoogleDoc, id)?;
                    if let Some(e) = meet.first() {
                        let mut r = IngestResult::new(
                            &w.raw,
                            IngestStatus::Duplicate,
                            Some(format!(
                                "already ingested as a google.meet entry ({})",
                                e.entry_uid
                            )),
                        )
                        .with_entry(e);
                        r.duplicate_of = Some(e.entry_uid.clone());
                        r
                    } else if let Some(e) = docs.first() {
                        IngestResult::new(&w.raw, IngestStatus::WouldUpdate, None).with_entry(e)
                    } else if !cat_accounts.iter().any(|a| {
                        a.kind == AccountKind::Google
                            && a.status == AccountStatus::Active
                            && opts.account.as_deref().is_none_or(|x| a.id.as_str() == x)
                    }) {
                        IngestResult::failed(&w.raw, "no usable Google account")
                    } else {
                        let mut r = IngestResult::new(&w.raw, IngestStatus::WouldCreate, None);
                        r.source_kind = Some(kind.to_string());
                        r
                    }
                }
                other => {
                    let account = if matches!(other, Locator::Web(_)) {
                        "web"
                    } else {
                        "local"
                    };
                    let acc = cat_accounts.iter().find(|a| a.id.as_str() == account);
                    let source = match acc {
                        Some(a) => self.source_for(a)?,
                        None => None,
                    };
                    let resolved = source.as_ref().and_then(|s| s.resolve(&w.raw));
                    match resolved {
                        None => IngestResult::failed(
                            &w.raw,
                            if matches!(other, Locator::Local(_)) {
                                "file not found or not readable"
                            } else {
                                "cannot resolve the URL (run `sb setup home` if the web account is missing)"
                            },
                        ),
                        Some(sref)
                            if source
                                .as_ref()
                                .is_some_and(|s| s.refusal(&sref.source_id).is_some()) =>
                        {
                            IngestResult::failed(&w.raw, "path is not allowed")
                        }
                        Some(sref) => {
                            let existing = self.catalog().entry_by_key(
                                sref.account_id.as_str(),
                                kind,
                                &sref.source_id,
                            )?;
                            match existing {
                                Some(e) => {
                                    IngestResult::new(&w.raw, IngestStatus::WouldUpdate, None)
                                        .with_entry(&e)
                                }
                                None => {
                                    let mut r =
                                        IngestResult::new(&w.raw, IngestStatus::WouldCreate, None);
                                    r.source_kind = Some(kind.to_string());
                                    r.account = Some(sref.account_id.to_string());
                                    r.source_url = sref.source_url;
                                    r
                                }
                            }
                        }
                    }
                }
            };
            slots[w.idx] = Some(res);
        }
        Ok(())
    }
}

fn locator_text(l: &Locator) -> String {
    match l {
        Locator::Google(id) => format!("https://docs.google.com/document/d/{id}/edit"),
        Locator::Web(u) => u.clone(),
        Locator::Local(p) => p.to_string_lossy().into_owned(),
    }
}

fn has_drive(a: &Account) -> bool {
    match a.config.get("features").and_then(Value::as_array) {
        Some(f) => f
            .iter()
            .filter_map(Value::as_str)
            .any(|s| s == "docs" || s == "meet"),
        None => true,
    }
}

/// Whether the given title, context or date differs from what is stored.
fn overrides_differ(e: &Entry, hint: &IngestHint) -> bool {
    let stored: BTreeMap<&str, &Value> = e
        .metadata
        .as_object()
        .map(|m| m.iter().map(|(k, v)| (k.as_str(), v)).collect())
        .unwrap_or_default();
    hint.metadata()
        .iter()
        .any(|(k, v)| stored.get(k.as_str()).is_none_or(|s| *s != v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(
            classify("https://docs.google.com/document/d/1AbC-_9/edit?usp=sharing"),
            Ok(Locator::Google("1AbC-_9".into()))
        );
        assert_eq!(
            classify("https://docs.google.com/spreadsheets/u/0/d/SHEET1/edit#gid=0"),
            Ok(Locator::Google("SHEET1".into()))
        );
        assert_eq!(
            classify("https://drive.google.com/file/d/FILE9/view"),
            Ok(Locator::Google("FILE9".into()))
        );
        assert_eq!(
            classify("https://drive.google.com/open?id=OPEN1"),
            Ok(Locator::Google("OPEN1".into()))
        );
        assert!(matches!(
            classify("https://docs.google.com/document/d/e/2PACX-published/pub"),
            Ok(Locator::Web(_))
        ));
        assert!(
            classify("https://drive.google.com/drive/folders/XYZ")
                .unwrap_err()
                .contains("folders")
        );
        assert!(
            classify("https://acme.slack.com/archives/C1/p1")
                .unwrap_err()
                .contains("sb sync")
        );
        assert!(matches!(
            classify("https://example.com/a?b=1"),
            Ok(Locator::Web(_))
        ));
        assert_eq!(
            classify("notes/a.md"),
            Ok(Locator::Local("notes/a.md".into()))
        );
        assert_eq!(
            classify(r"C:\notes\a.md"),
            Ok(Locator::Local(r"C:\notes\a.md".into()))
        );
        assert!(
            classify("ftp://example.com/a")
                .unwrap_err()
                .contains("scheme")
        );
        assert!(classify("  ").is_err());
        #[cfg(unix)]
        assert_eq!(
            classify("file:///home/u/a%20b.md"),
            Ok(Locator::Local("/home/u/a b.md".into()))
        );
    }
}
