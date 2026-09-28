//! The summarize stage (`sb summarize`, the last stage of `sb sync`) and
//! `sb resummarize` (summarization.md, ADR-0005, ADR-0012).

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt;
use sb_core::summarizer::{LlmError, Summarizer};
use sb_core::{EntryOrigin, NormalizeOutcome, RawStatus, Severity, SummaryInput, SummaryStatus};
use sb_llm::prices::{ESTIMATED_OUTPUT_TOKENS, estimate_tokens, price_for, usage_cost};
use sb_llm::prompts::split_chunks;
use sb_llm::{BuildOptions, Built, NATIVE};
use sb_store::{Entry, EntryFilter, SummaryCommit, SummaryDecision};
use serde::Serialize;

use crate::Pipeline;
use crate::error::PipelineError;
use crate::host::Host;
use crate::policy::SummaryPolicy;
use crate::run::{Limits, Progress, Stop, SummaryStats};

/// What to summarize with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The profile configured for each source kind (`summary.profile.*`).
    Configured,
    /// A named profile for every selected entry.
    Profile(String),
    /// Restore the source-native summary from raw data (`--native`).
    Native,
}

/// Options for a summarize or resummarize run.
#[derive(Debug, Clone)]
pub struct SummarizeOptions {
    pub filter: EntryFilter,
    pub target: Target,
    pub limits: Limits,
    /// `sb summarize --retry-failed`: reset attempt counters first.
    pub retry_failed: bool,
    /// `sb resummarize --force`: redo entries already at the target generator.
    pub force: bool,
    /// Count and estimate only; no LLM calls, no writes.
    pub estimate_only: bool,
}

/// Cost estimate.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Estimate {
    pub entries: u64,
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// `None` when no price is known for some model.
    pub cost_usd: Option<f64>,
    pub unpriced_models: Vec<String>,
    pub by_profile: BTreeMap<String, u64>,
}

/// Result of a summarize or resummarize run.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SummarizeReport {
    pub selected: u64,
    pub stats: SummaryStats,
    pub stop: Option<Stop>,
    pub estimate: Option<Estimate>,
    /// Entries left for later (no usable profile, server unreachable, ...).
    pub deferred: u64,
    pub errors: Vec<String>,
}

struct Job<'e> {
    entry: &'e Entry,
    profile: String,
}

/// Shared mutable state of a run.
struct RunState {
    stats: SummaryStats,
    started: u64,
    stop: Option<Stop>,
    errors: Vec<String>,
}

impl Pipeline {
    fn build_profile(&self, policy: &SummaryPolicy, name: &str) -> Result<Built, LlmError> {
        let profile = policy.profile(name).ok_or_else(|| {
            let why = policy
                .invalid_profiles
                .get(name)
                .map(|r| format!(": {r}"))
                .unwrap_or_default();
            LlmError::Config(format!("profile {name:?} is not defined{why}"))
        })?;
        let secret = match profile.secret_ref() {
            Some(r) => self
                .catalog()
                .resolve_secret_ref(&r)
                .map_err(|e| LlmError::Config(e.to_string()))?,
            None => None,
        };
        sb_llm::build(
            name,
            profile,
            &BuildOptions {
                language: policy.language.clone(),
                scratch_dir: self.home().tmp_dir(),
                secret,
            },
        )
    }

    /// Hosts per account, for normalization.
    fn hosts<'a>(
        &'a self,
        policy: &'a SummaryPolicy,
        entries: &[Entry],
    ) -> Result<HashMap<String, Host<'a>>, PipelineError> {
        let mut hosts = HashMap::new();
        for e in entries {
            if hosts.contains_key(&e.account_id) {
                continue;
            }
            let account = self.account(&e.account_id)?;
            if let Some(src) = self.source_for(&account)? {
                hosts.insert(
                    e.account_id.clone(),
                    Host::new(self, account, src, policy, EntryOrigin::Sync)?,
                );
            }
        }
        Ok(hosts)
    }

    /// Rebuild the summary input of an entry from raw data.
    fn rebuild_input(
        &self,
        hosts: &HashMap<String, Host<'_>>,
        e: &Entry,
    ) -> Result<Option<SummaryInput>, PipelineError> {
        let Some(host) = hosts.get(&e.account_id) else {
            return Ok(None);
        };
        match host.normalize_stored(e)? {
            NormalizeOutcome::Entry(n) => Ok(n.summary_input),
            NormalizeOutcome::NotApplicable(_) => Ok(None),
        }
    }

    /// `sb summarize` and the summarize stage of `sb sync`.
    pub async fn summarize_pending(
        &self,
        opts: &SummarizeOptions,
    ) -> Result<SummarizeReport, PipelineError> {
        let policy = SummaryPolicy::load(&self.catalog())?;
        if opts.retry_failed && !opts.estimate_only {
            self.catalog().reset_summary_attempts(&opts.filter)?;
        }
        let candidates = self
            .catalog()
            .summarization_candidates(&opts.filter, policy.max_attempts)?;
        let mut report = SummarizeReport {
            selected: candidates.len() as u64,
            ..Default::default()
        };
        let mut jobs = Vec::new();
        for e in &candidates {
            let name = match &opts.target {
                Target::Profile(p) => Some(p.clone()),
                _ => policy.profile_name(e.source_kind).map(str::to_string),
            };
            match name {
                Some(n) if n != NATIVE => jobs.push(Job {
                    entry: e,
                    profile: n,
                }),
                _ => report.deferred += 1,
            }
        }
        if report.deferred > 0 {
            self.emit(Progress::Warning(format!(
                "{} pending entries have no summarizer profile (set summary.profile.default)",
                report.deferred
            )));
        }
        self.run_jobs(&policy, jobs, opts, report, false).await
    }

    /// `sb resummarize`.
    pub async fn resummarize(
        &self,
        opts: &SummarizeOptions,
    ) -> Result<SummarizeReport, PipelineError> {
        let policy = SummaryPolicy::load(&self.catalog())?;
        let entries = self.catalog().list_entries(&EntryFilter {
            limit: None,
            ..opts.filter.clone()
        })?;
        let limit = opts.filter.limit;
        let mut report = SummarizeReport::default();
        let mut eligible: Vec<&Entry> = Vec::new();
        for e in &entries {
            if e.raw_status != RawStatus::Present {
                report.stats.no_raw += 1;
                continue;
            }
            eligible.push(e);
        }
        if opts.target == Target::Native {
            return self.restore_native(&policy, eligible, opts, report).await;
        }
        let Target::Profile(profile_name) = &opts.target else {
            return Err(PipelineError::Invalid(
                "resummarize needs --profile or --native".into(),
            ));
        };
        let profile = policy
            .profile(profile_name)
            .ok_or_else(|| {
                PipelineError::Invalid(format!("profile {profile_name:?} is not defined"))
            })?
            .clone();
        let hosts = self.hosts(&policy, &entries)?;
        let target_version =
            |input: &SummaryInput| sb_llm::prompts::prompt_version(input.prompt).to_string();
        let mut jobs = Vec::new();
        for e in eligible {
            if !opts.force {
                // Skip entries already at the target generator with the same input.
                let current = self.catalog().summary(e.id)?;
                if let Some(s) = current
                    && s.provider == profile.provider.recorded_name()
                    && s.model == profile.model_name()
                    && let Some(input) = self.rebuild_input(&hosts, e)?
                    && s.prompt_version.as_deref() == Some(target_version(&input).as_str())
                    && s.input_hash == policy.input_hash(&input, &profile)
                {
                    report.stats.already_current += 1;
                    continue;
                }
            }
            jobs.push(Job {
                entry: e,
                profile: profile_name.clone(),
            });
            if limit.is_some_and(|l| jobs.len() as u64 >= l) {
                break;
            }
        }
        report.selected = jobs.len() as u64;
        drop(hosts);
        self.run_jobs(&policy, jobs, opts, report, true).await
    }

    async fn restore_native(
        &self,
        policy: &SummaryPolicy,
        entries: Vec<&Entry>,
        opts: &SummarizeOptions,
        mut report: SummarizeReport,
    ) -> Result<SummarizeReport, PipelineError> {
        let owned: Vec<Entry> = entries.iter().map(|e| (*e).clone()).collect();
        let hosts = self.hosts(policy, &owned)?;
        report.selected = owned.len() as u64;
        for e in &owned {
            if self.is_cancelled() {
                report.stop = Some(Stop::Cancelled);
                break;
            }
            let Some(host) = hosts.get(&e.account_id) else {
                continue;
            };
            let outcome = host.normalize_stored(e)?;
            let NormalizeOutcome::Entry(n) = &outcome else {
                report.stats.skipped += 1;
                continue;
            };
            let Some(generator) = n.native_summary.clone() else {
                report.stats.skipped += 1;
                continue;
            };
            if opts.estimate_only {
                report.stats.summarized += 1;
                continue;
            }
            if let Some(mut u) = host.renormalized_update(e, outcome.clone())? {
                if let Some(nu) = u.normalized.as_mut() {
                    let body = n
                        .summary_input
                        .as_ref()
                        .map(|i| i.body.as_str())
                        .unwrap_or("");
                    nu.summary = SummaryDecision::Native {
                        generator,
                        input_hash: sb_core::util::summary_input_hash(NATIVE, "", body),
                    };
                }
                self.catalog().upsert_entry(&u)?;
                report.stats.summarized += 1;
            }
        }
        Ok(report)
    }

    async fn run_jobs(
        &self,
        policy: &SummaryPolicy,
        jobs: Vec<Job<'_>>,
        opts: &SummarizeOptions,
        mut report: SummarizeReport,
        replace_existing: bool,
    ) -> Result<SummarizeReport, PipelineError> {
        let owned: Vec<Entry> = jobs.iter().map(|j| j.entry.clone()).collect();
        let hosts = self.hosts(policy, &owned)?;

        // Group by profile, keeping newest-first order within each group.
        let mut groups: BTreeMap<String, Vec<&Entry>> = BTreeMap::new();
        for j in &jobs {
            groups.entry(j.profile.clone()).or_default().push(j.entry);
        }

        if opts.estimate_only {
            let mut est = Estimate {
                cost_usd: Some(0.0),
                ..Default::default()
            };
            for (name, entries) in &groups {
                let profile = policy.profile(name);
                let max_chars = profile
                    .map(|p| p.max_input_chars)
                    .unwrap_or(sb_llm::profile::DEFAULT_MAX_INPUT_CHARS);
                let model = profile.map(|p| p.model_name()).unwrap_or_default();
                let price = price_for(&model, policy.prices.as_ref());
                let cli = profile.is_some_and(|p| {
                    p.cli_binary().is_some() || p.kind == sb_core::GeneratorKind::LocalLlm
                });
                for e in entries {
                    let Some(input) = self.rebuild_input(&hosts, e)? else {
                        continue;
                    };
                    if !replace_existing && policy.below_thresholds(&input) {
                        continue;
                    }
                    let chunks = split_chunks(&input.body, max_chars).len() as u64;
                    let calls = if chunks > 1 { chunks + 1 } else { 1 };
                    let in_tok = estimate_tokens(&input.body) + 400 * calls;
                    let out_tok = ESTIMATED_OUTPUT_TOKENS * calls;
                    est.entries += 1;
                    est.calls += calls;
                    est.input_tokens += in_tok;
                    est.output_tokens += out_tok;
                    *est.by_profile.entry(name.clone()).or_default() += 1;
                    match (price, cli) {
                        (_, true) => {}
                        (Some(p), false) => {
                            if let Some(c) = est.cost_usd.as_mut() {
                                *c += (in_tok as f64 * p.input + out_tok as f64 * p.output) / 1e6;
                            }
                        }
                        (None, false) => {
                            if !est.unpriced_models.contains(&model) {
                                est.unpriced_models.push(model.clone());
                            }
                        }
                    }
                }
            }
            if !est.unpriced_models.is_empty() {
                est.cost_usd = None;
            }
            report.estimate = Some(est);
            return Ok(report);
        }

        let total: u64 = groups.values().map(|g| g.len() as u64).sum();
        let state = Mutex::new(RunState {
            stats: std::mem::take(&mut report.stats),
            started: 0,
            stop: None,
            errors: Vec::new(),
        });
        let grace = Duration::from_secs(
            self.catalog()
                .setting_or("pipeline.shutdown_grace_secs", 30u64)?,
        );

        for (name, entries) in groups {
            if state.lock().map(|s| s.stop.is_some()).unwrap_or(true) {
                report.deferred += entries.len() as u64;
                continue;
            }
            let built = match self.build_profile(policy, &name) {
                Ok(b) => b,
                Err(e) => {
                    let msg = format!("summarizer profile {name}: {e}");
                    self.catalog()
                        .open_issue(e.issue_code(), Severity::Error, None, None, &msg)?;
                    self.emit(Progress::Warning(msg.clone()));
                    report.errors.push(msg);
                    report.deferred += entries.len() as u64;
                    continue;
                }
            };
            match built.prepare().await {
                Ok(_) => {
                    self.catalog()
                        .resolve_issues("llm.local_unreachable", None)?;
                }
                Err(e) => {
                    let msg = format!("summarizer profile {name}: {e}; summaries stay pending");
                    self.catalog().open_issue(
                        "llm.local_unreachable",
                        Severity::Warning,
                        None,
                        None,
                        &msg,
                    )?;
                    self.emit(Progress::Warning(msg.clone()));
                    report.errors.push(msg);
                    report.deferred += entries.len() as u64;
                    continue;
                }
            }
            self.catalog().resolve_issues("llm.config", None)?;
            self.catalog().resolve_issues("llm.auth", None)?;
            let price = price_for(&built.profile.model_name(), policy.prices.as_ref());
            let aborted = AtomicBool::new(false);
            let concurrency = built.profile.concurrency.max(1);

            let work = futures::stream::iter(entries)
                .map(|e| {
                    let built = &built;
                    let state = &state;
                    let hosts = &hosts;
                    let aborted = &aborted;
                    async move {
                        // Check limits and cancellation before starting a unit.
                        {
                            let Ok(mut s) = state.lock() else { return };
                            if s.stop.is_some() || aborted.load(Ordering::SeqCst) {
                                return;
                            }
                            if self.is_cancelled() {
                                s.stop = Some(Stop::Cancelled);
                                return;
                            }
                            if let Some(m) = opts.limits.max_summaries
                                && s.started >= m
                            {
                                s.stop = Some(Stop::Limit(format!("--max-summaries {m}")));
                                return;
                            }
                            if let Some(c) = opts.limits.max_cost_usd
                                && s.stats.cost_usd >= c
                            {
                                s.stop = Some(Stop::Limit(format!("--max-cost {c}")));
                                return;
                            }
                            if opts.limits.time_exceeded() {
                                s.stop = Some(Stop::Limit("--time-limit".into()));
                                return;
                            }
                            s.started += 1;
                        }
                        let res = self
                            .summarize_one(policy, hosts, built, e, replace_existing)
                            .await;
                        let Ok(mut s) = state.lock() else { return };
                        match res {
                            Ok(Outcome::Done(usage)) => {
                                s.stats.summarized += 1;
                                s.stats.input_tokens += usage.input_tokens;
                                s.stats.output_tokens += usage.output_tokens;
                                match usage_cost(&usage, price) {
                                    Some(c) => s.stats.cost_usd += c,
                                    None if built.profile.kind
                                        == sb_core::GeneratorKind::LlmApi =>
                                    {
                                        s.stats.unpriced += 1
                                    }
                                    None => {}
                                }
                            }
                            Ok(Outcome::Skipped) => s.stats.skipped += 1,
                            Err(err) => {
                                s.stats.failed += 1;
                                if matches!(
                                    err,
                                    PipelineError::Llm(LlmError::Auth(_) | LlmError::Config(_))
                                ) {
                                    aborted.store(true, Ordering::SeqCst);
                                }
                                s.errors.push(format!("{}: {err}", e.entry_uid));
                            }
                        }
                        let done = s.stats.summarized + s.stats.failed + s.stats.skipped;
                        self.emit(Progress::Summarized {
                            done,
                            remaining: total.saturating_sub(done),
                            cost_usd: s.stats.cost_usd,
                        });
                    }
                })
                .buffer_unordered(concurrency)
                .collect::<Vec<()>>();

            tokio::select! {
                _ = work => {}
                _ = async {
                    self.cancel.cancelled().await;
                    tokio::time::sleep(grace).await;
                } => {
                    tracing::warn!("grace period elapsed; abandoning in-flight summaries");
                    if let Ok(mut s) = state.lock() {
                        s.stop = Some(Stop::Cancelled);
                    }
                }
            }
        }
        let s = state
            .into_inner()
            .map_err(|_| PipelineError::Invalid("state poisoned".into()))?;
        report.stats = s.stats;
        report.stop = s.stop;
        report.errors.extend(s.errors);
        if report.stop.is_none() && self.is_cancelled() {
            report.stop = Some(Stop::Cancelled);
        }
        report.stats.remaining = self
            .catalog()
            .summarization_candidates(&EntryFilter::default(), policy.max_attempts)?
            .len() as u64;
        Ok(report)
    }

    /// Summarize one entry and commit the result on its own.
    async fn summarize_one(
        &self,
        policy: &SummaryPolicy,
        hosts: &HashMap<String, Host<'_>>,
        built: &Built,
        e: &Entry,
        replace_existing: bool,
    ) -> Result<Outcome, PipelineError> {
        let input = match self.rebuild_input(hosts, e) {
            Ok(Some(i)) => i,
            Ok(None) => {
                if !replace_existing {
                    self.catalog()
                        .set_summary_status(e.id, SummaryStatus::None)?;
                }
                return Ok(Outcome::Skipped);
            }
            Err(err) => {
                self.catalog()
                    .record_summary_failure(e.id, &err.to_string())?;
                return Err(err);
            }
        };
        if !replace_existing && policy.below_thresholds(&input) {
            self.catalog()
                .set_summary_status(e.id, SummaryStatus::Skipped)?;
            return Ok(Outcome::Skipped);
        }
        let hash = policy.input_hash(&input, &built.profile);
        let generator = built.summarizer.generator(&input);
        match built.summarizer.summarize(&input).await {
            Ok(out) => {
                let usage = out.usage.clone();
                self.catalog().commit_summary(&SummaryCommit {
                    entry_id: e.id,
                    sections: out.to_sections(input.want_details),
                    generator,
                    profile: Some(built.name.clone()),
                    input_hash: hash,
                    usage: usage.clone(),
                })?;
                let cat = self.catalog();
                cat.resolve_entry_issues("llm.failed", e.id)?;
                cat.resolve_entry_issues("llm.bad_output", e.id)?;
                Ok(Outcome::Done(usage))
            }
            Err(err) => {
                let cat = self.catalog();
                match &err {
                    // Configuration problems are not the entry's fault: do not
                    // count an attempt.
                    LlmError::Auth(_) | LlmError::Config(_) => {
                        cat.open_issue(
                            err.issue_code(),
                            Severity::Error,
                            None,
                            None,
                            &format!("{}: {err}", built.name),
                        )?;
                    }
                    _ => {
                        cat.record_summary_failure(e.id, &err.to_string())?;
                        cat.open_issue(
                            err.issue_code(),
                            Severity::Warning,
                            Some(&e.account_id),
                            Some(e.id),
                            &format!("summarizing \"{}\" failed: {err}", e.title),
                        )?;
                    }
                }
                Err(err.into())
            }
        }
    }
}

enum Outcome {
    Done(sb_core::Usage),
    Skipped,
}
