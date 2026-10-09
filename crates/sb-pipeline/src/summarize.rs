//! The summarize stage (`sb summarize`, the last stage of `sb sync`) and
//! `sb resummarize` (summarization.md, ADR-0005, ADR-0012).

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt;
use second_brain_kernel::summarizer::{LlmError, Summarizer};
use second_brain_kernel::{
    EntryOrigin, GeneratorKind, NormalizeOutcome, RawStatus, Severity, SummaryInput, SummaryStatus,
    Usage,
};
use second_brain_llm::prices::{
    ESTIMATED_OUTPUT_TOKENS, Price, estimate_tokens, price_for, usage_cost,
};
use second_brain_llm::prompts::split_chunks;
use second_brain_llm::{BuildOptions, Built, NATIVE};
use second_brain_store::{
    Entry, EntryFilter, NewUsage, SummaryCommit, SummaryDecision, UsageOutcome,
};
use serde::Serialize;

use crate::Pipeline;
use crate::budget::{Blocked, BudgetGate, Gate};
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
    /// How the estimate compares with the remaining budget (ADR-0013); `None`
    /// when no cap is enabled or nothing in the estimate is paid.
    pub budget: Option<BudgetEstimate>,
}

/// An estimate against the remaining weekly and monthly budget.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct BudgetEstimate {
    /// Left under the strictest cap right now.
    pub remaining_usd: f64,
    /// Entries summarized by a priced paid profile.
    pub paid_entries: u64,
    /// How many of them fit, taken in the order they would be processed.
    pub entries_that_fit: u64,
    pub fits: bool,
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
    /// Entries too large for the budget, left `pending`.
    too_large: u64,
    /// The `llm.budget_exhausted` issue has been resolved in this run.
    budget_issue_resolved: bool,
}

/// The calls, tokens and cost one summary is expected to take (the estimate
/// shared by `--estimate` and the budget gate).
fn unit_estimate(input: &SummaryInput, max_chars: usize) -> (u64, u64, u64) {
    let chunks = split_chunks(&input.body, max_chars).len() as u64;
    let calls = if chunks > 1 { chunks + 1 } else { 1 };
    let in_tok = estimate_tokens(&input.body) + 400 * calls;
    let out_tok = ESTIMATED_OUTPUT_TOKENS * calls;
    (calls, in_tok, out_tok)
}

fn unit_cost(price: Option<Price>, in_tok: u64, out_tok: u64) -> Option<f64> {
    price.map(|p| (in_tok as f64 * p.input + out_tok as f64 * p.output) / 1e6)
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
        second_brain_llm::build(
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
    pub(crate) fn hosts<'a>(
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
    pub(crate) fn rebuild_input(
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
        if !opts.estimate_only {
            self.upgrade_input_hashes()?;
        }
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
        if !opts.estimate_only {
            self.upgrade_input_hashes()?;
        }
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
        policy.profile(profile_name).ok_or_else(|| {
            PipelineError::Invalid(format!("profile {profile_name:?} is not defined"))
        })?;
        let hosts = self.hosts(&policy, &entries)?;
        let target_version = |input: &SummaryInput| {
            second_brain_llm::prompts::prompt_version(input.prompt).to_string()
        };
        let mut jobs = Vec::new();
        for e in eligible {
            if !opts.force {
                // Skip entries already at the target generator with the same input.
                let current = self.catalog().summary(e.id)?;
                if let Some(s) = current
                    // The recorded model is not compared (ADR-0017): a model change
                    // is applied explicitly, with `--force` or a new profile.
                    && s.profile.as_deref() == Some(profile_name.as_str())
                    && let Some(input) = self.rebuild_input(&hosts, e)?
                    && s.prompt_version.as_deref() == Some(target_version(&input).as_str())
                    && s.input_hash == policy.input_hash(&input)
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
                        input_hash: second_brain_kernel::util::summary_input_hash(NATIVE, "", body),
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
            let gate = BudgetGate::from_policy(policy);
            let remaining = if gate.enabled() {
                gate.remaining(&self.catalog())?
            } else {
                None
            };
            let (mut paid_entries, mut fit, mut spent_est, mut still_fits) =
                (0u64, 0u64, 0.0f64, true);
            for (name, entries) in &groups {
                let profile = policy.profile(name);
                let max_chars = profile
                    .map(|p| p.max_input_chars)
                    .unwrap_or(second_brain_llm::profile::DEFAULT_MAX_INPUT_CHARS);
                let model = profile.map(|p| p.model_name()).unwrap_or_default();
                let price = price_for(&model, policy.prices.as_ref());
                let cli = profile.is_some_and(|p| {
                    p.cli_binary().is_some()
                        || p.kind == second_brain_kernel::GeneratorKind::LocalLlm
                });
                for e in entries {
                    let Some(input) = self.rebuild_input(&hosts, e)? else {
                        continue;
                    };
                    if !replace_existing && policy.below_thresholds(&input) {
                        continue;
                    }
                    let (calls, in_tok, out_tok) = unit_estimate(&input, max_chars);
                    est.entries += 1;
                    est.calls += calls;
                    est.input_tokens += in_tok;
                    est.output_tokens += out_tok;
                    *est.by_profile.entry(name.clone()).or_default() += 1;
                    match (price, cli) {
                        (_, true) => {}
                        (Some(_), false) => {
                            let unit = unit_cost(price, in_tok, out_tok).unwrap_or(0.0);
                            if let Some(c) = est.cost_usd.as_mut() {
                                *c += unit;
                            }
                            if let Some(rem) = remaining {
                                paid_entries += 1;
                                if still_fits && spent_est + unit <= rem {
                                    spent_est += unit;
                                    fit += 1;
                                } else {
                                    still_fits = false;
                                }
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
            if let Some(rem) = remaining
                && paid_entries > 0
            {
                est.budget = Some(BudgetEstimate {
                    remaining_usd: rem,
                    paid_entries,
                    entries_that_fit: fit,
                    fits: fit == paid_entries,
                });
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
            too_large: 0,
            budget_issue_resolved: false,
        });
        let gate = BudgetGate::from_policy(policy);
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
            let paid = built.profile.kind != GeneratorKind::LocalLlm;
            if paid {
                if gate.enabled() {
                    gate.record_periods(&self.catalog())?;
                }
                // A cap that cannot be measured is not a cap (ADR-0013).
                if gate.enabled() && built.profile.kind == GeneratorKind::LlmApi && price.is_none()
                {
                    let msg = format!(
                        "summarizer profile {name}: no price is known for model {}, so the budget cannot be enforced; set llm.prices or disable the caps (summary.budget.*)",
                        built.profile.model_name()
                    );
                    self.catalog()
                        .open_issue("llm.unpriced", Severity::Error, None, None, &msg)?;
                    self.emit(Progress::Warning(msg.clone()));
                    report.errors.push(msg);
                    report.deferred += entries.len() as u64;
                    continue;
                }
                self.catalog().resolve_issues("llm.unpriced", None)?;
            }
            let aborted = AtomicBool::new(false);
            let concurrency = built.profile.concurrency.max(1);

            let work = futures::stream::iter(entries)
                .map(|e| {
                    let built = &built;
                    let state = &state;
                    let hosts = &hosts;
                    let gate = &gate;
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
                            .summarize_one(policy, hosts, built, gate, price, e, replace_existing)
                            .await;
                        let Ok(mut s) = state.lock() else { return };
                        match res {
                            Ok(Outcome::Done(usage, cost)) => {
                                s.stats.summarized += 1;
                                s.stats.input_tokens += usage.input_tokens;
                                s.stats.output_tokens += usage.output_tokens;
                                match cost {
                                    Some(c) => s.stats.cost_usd += c,
                                    None if built.profile.kind == GeneratorKind::LlmApi => {
                                        s.stats.unpriced += 1
                                    }
                                    None => {}
                                }
                                if !s.budget_issue_resolved {
                                    s.budget_issue_resolved = true;
                                    if let Err(err) = self
                                        .catalog()
                                        .resolve_issues("llm.budget_exhausted", None)
                                    {
                                        s.errors.push(format!("{err}"));
                                    }
                                }
                            }
                            Ok(Outcome::Skipped) => s.stats.skipped += 1,
                            Ok(Outcome::Blocked(b)) => {
                                // Not started: it neither counts as started nor fails.
                                s.started = s.started.saturating_sub(1);
                                if s.stop.is_none() {
                                    s.stop = Some(Stop::Limit(b.stop_detail()));
                                    let msg = b.message();
                                    let cat = self.catalog();
                                    if let Err(err) = gate
                                        .record_stop(&cat, &b)
                                        .and_then(|_| {
                                            cat.open_issue(
                                                "llm.budget_exhausted",
                                                Severity::Warning,
                                                None,
                                                None,
                                                &msg,
                                            )
                                            .map(|_| ())
                                            .map_err(Into::into)
                                        })
                                    {
                                        s.errors.push(format!("{err}"));
                                    }
                                    drop(cat);
                                    self.emit(Progress::Warning(msg));
                                }
                            }
                            Ok(Outcome::TooLarge {
                                needed_usd,
                                cap_usd,
                            }) => {
                                s.started = s.started.saturating_sub(1);
                                s.too_large += 1;
                                if s.too_large == 1 {
                                    self.emit(Progress::Warning(format!(
                                        "{} alone is estimated at ${needed_usd:.2}, more than a whole budget period (${cap_usd:.2}); it stays pending",
                                        e.entry_uid
                                    )));
                                }
                            }
                            Ok(Outcome::Failed { err, usage, cost }) => {
                                // A billed attempt still counts against the run.
                                s.stats.failed += 1;
                                s.stats.input_tokens += usage.input_tokens;
                                s.stats.output_tokens += usage.output_tokens;
                                if let Some(c) = cost {
                                    s.stats.cost_usd += c;
                                }
                                if matches!(
                                    err,
                                    PipelineError::Llm(LlmError::Auth(_) | LlmError::Config(_))
                                ) {
                                    aborted.store(true, Ordering::SeqCst);
                                }
                                s.errors.push(format!("{}: {err}", e.entry_uid));
                            }
                            Err(err) => {
                                s.stats.failed += 1;
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
        report.deferred += s.too_large;
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
    #[allow(clippy::too_many_arguments)]
    async fn summarize_one(
        &self,
        policy: &SummaryPolicy,
        hosts: &HashMap<String, Host<'_>>,
        built: &Built,
        gate: &BudgetGate,
        price: Option<Price>,
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
        // The thresholds are checked again here, so a raised `summary.min_chars`
        // also applies to entries that are already pending. No LLM call is made.
        if !replace_existing && policy.below_thresholds(&input) {
            self.catalog()
                .set_summary_status(e.id, SummaryStatus::Skipped)?;
            return Ok(Outcome::Skipped);
        }
        let hash = policy.input_hash(&input);
        let mut generator = built.summarizer.generator(&input);

        // The budget gate (ADR-0013): local models are free and never gated.
        let paid = built.profile.kind != GeneratorKind::LocalLlm;
        let _reservation = if paid && gate.enabled() {
            let (_, in_tok, out_tok) = unit_estimate(&input, built.profile.max_input_chars);
            // An unknown price (a CLI that reports its own cost) reserves nothing,
            // but still cannot start on an exhausted budget.
            let needed = unit_cost(price, in_tok, out_tok).unwrap_or(0.0);
            match gate.try_reserve(&self.catalog(), needed)? {
                Gate::Go(r) => Some(r),
                Gate::Stop(b) => return Ok(Outcome::Blocked(b)),
                Gate::TooLarge {
                    needed_usd,
                    cap_usd,
                    ..
                } => {
                    return Ok(Outcome::TooLarge {
                        needed_usd,
                        cap_usd,
                    });
                }
            }
        } else {
            None
        };

        let mut usage = Usage::default();
        let result = built.summarizer.summarize_tracked(&input, &mut usage).await;
        // A CLI that resolved an alias (`haiku`) reports the model it really used;
        // that is what gets recorded, also for billed calls that failed (ADR-0017).
        if let Some(m) = usage.model.clone() {
            generator.model = m;
        }
        // Local models cost nothing; otherwise the provider's cost, else tokens
        // times price, else unknown.
        let cost = if paid {
            usage_cost(&usage, price)
        } else {
            Some(0.0)
        };
        match result {
            Ok(mut out) => {
                out.usage = usage.clone();
                self.catalog().commit_summary(&SummaryCommit {
                    entry_id: e.id,
                    sections: out.to_sections(input.want_details),
                    generator,
                    profile: Some(built.name.clone()),
                    input_hash: hash,
                    usage: usage.clone(),
                    cost_usd: cost,
                    run_id: self.run_id(),
                })?;
                let cat = self.catalog();
                cat.resolve_entry_issues("llm.failed", e.id)?;
                cat.resolve_entry_issues("llm.bad_output", e.id)?;
                Ok(Outcome::Done(usage, cost))
            }
            Err(err) => {
                let cat = self.catalog();
                // Calls that were made before the failure are billed: record them.
                if usage.calls > 0 {
                    cat.record_usage(&NewUsage {
                        run_id: self.run_id(),
                        entry_id: Some(e.id),
                        profile: built.name.clone(),
                        generator,
                        usage: usage.clone(),
                        cost_usd: cost,
                        outcome: UsageOutcome::Failed,
                    })?;
                }
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
                Ok(Outcome::Failed {
                    err: err.into(),
                    usage,
                    cost,
                })
            }
        }
    }
}

enum Outcome {
    /// Summarized and committed; the usage and its cost (`None` = unknown).
    Done(Usage, Option<f64>),
    Skipped,
    /// A budget cap is used up: stop the stage (ADR-0013).
    Blocked(Blocked),
    /// Too expensive for any period of the budget: left pending.
    TooLarge {
        needed_usd: f64,
        cap_usd: f64,
    },
    /// The summarizer failed. `usage` and `cost` are what the attempt used.
    Failed {
        err: PipelineError,
        usage: Usage,
        cost: Option<f64>,
    },
}
