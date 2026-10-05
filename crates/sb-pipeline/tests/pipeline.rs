//! Pipeline tests against a fake source and a mock OpenAI-compatible LLM.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::type_complexity)]
// Guards are dropped explicitly before awaiting; clippy cannot see `drop`.
#![allow(clippy::await_holding_lock)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sb_core::source::{Source, SourceError, SyncHost};
use sb_core::*;
use sb_pipeline::summarize::{SummarizeOptions, Target};
use sb_pipeline::sync::SyncOptions;
use sb_pipeline::{Limits, Pipeline, PipelineError, SourceFactory};
use sb_store::{Account, Catalog, EntryFilter, Home};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Remote state: thread id → messages.
/// Recorded fetches: (source_id, fetch_state).
type Fetches = Arc<Mutex<Vec<(String, Option<Value>)>>>;

type Remote = Arc<Mutex<BTreeMap<String, Vec<String>>>>;

/// A fake thread source. Discovery enqueues every remote thread whose message
/// count exceeds the stored fetch state; fetch appends only new messages.
struct FakeSource {
    remote: Remote,
    fetches: Fetches,
    /// Cancel the run (via the token) after this many fetches.
    cancel_after: Option<(usize, tokio_util::sync::CancellationToken)>,
    fetched: AtomicUsize,
}

#[async_trait]
impl Source for FakeSource {
    fn kinds(&self) -> &'static [SourceKind] {
        &[SourceKind::SlackThread]
    }

    fn account_kind(&self) -> AccountKind {
        AccountKind::Slack
    }

    async fn sync(&self, host: &dyn SyncHost, _opts: &SyncOptions_) -> Result<(), SourceError> {
        let remote = self.remote.lock().unwrap().clone();
        let mut batch = DiscoveryBatch::default();
        for (id, msgs) in remote {
            let stored = host
                .fetch_state(SourceKind::SlackThread, &id)?
                .and_then(|v| v.get("count").and_then(Value::as_u64))
                .unwrap_or(0) as usize;
            if msgs.len() > stored {
                batch.enqueue.push(QueueItem {
                    source_kind: SourceKind::SlackThread,
                    source_id: id.clone(),
                    reason: "new_replies".into(),
                    hint: json!({}),
                });
            }
        }
        batch.cursors.push(CursorUpdate {
            source_kind: SourceKind::SlackThread,
            key: "scan".into(),
            value: Some(json!({"done": true})),
        });
        host.commit(batch)
    }

    async fn fetch(
        &self,
        _host: &dyn SyncHost,
        req: &FetchRequest,
    ) -> Result<FetchOutcome, SourceError> {
        let n = self.fetched.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some((after, token)) = &self.cancel_after
            && n >= *after
        {
            token.cancel();
        }
        self.fetches
            .lock()
            .unwrap()
            .push((req.source_id.clone(), req.fetch_state.clone()));
        let msgs = self
            .remote
            .lock()
            .unwrap()
            .get(&req.source_id)
            .cloned()
            .unwrap_or_default();
        let have = if req.full {
            0
        } else {
            req.fetch_state
                .as_ref()
                .and_then(|v| v.get("count").and_then(Value::as_u64))
                .unwrap_or(0) as usize
        };
        if msgs.len() <= have {
            return Ok(FetchOutcome::Unchanged);
        }
        let new: String = msgs[have..].iter().map(|m| format!("{m}\n")).collect();
        Ok(FetchOutcome::Fetched(Box::new(FetchedEntry {
            source_ref: SourceRef {
                account_id: AccountId::new("acme").unwrap(),
                source_kind: SourceKind::SlackThread,
                source_id: req.source_id.clone(),
                source_url: Some(format!("https://example.test/{}", req.source_id)),
                created_at: util::parse_ts("2026-09-01T00:00:00Z"),
                updated_at: None,
            },
            bundle: RawBundle {
                mode: if have == 0 {
                    RawMode::Replace
                } else {
                    RawMode::Append
                },
                objects: vec![RawObject {
                    role: RawRole::Primary,
                    media_type: "text/plain".into(),
                    ext: "txt".into(),
                    bytes: new.into_bytes(),
                }],
                fetch_state: Some(json!({"count": msgs.len()})),
                metadata: json!({"channel_name": "dev"}),
            },
        })))
    }

    fn normalize(
        &self,
        _ctx: &NormalizeCtx,
        input: &NormalizeInput,
    ) -> Result<NormalizeOutcome, SourceError> {
        let body: String = input
            .segments
            .iter()
            .map(|s| String::from_utf8_lossy(&s.bytes).to_string())
            .collect();
        let count = body.lines().count();
        Ok(NormalizeOutcome::Entry(Box::new(Normalized {
            title: format!("#dev {}", input.source_ref.source_id),
            source_url: None,
            source_created_at: input.source_ref.created_at,
            source_updated_at: None,
            metadata: json!({"message_count": count}),
            sections: vec![SectionDraft {
                kind: SectionKind::Details,
                origin: SectionOrigin::Extracted,
                text: body.clone(),
            }],
            summary_input: Some(SummaryInput {
                source_kind: SourceKind::SlackThread,
                prompt: PromptKind::Conversation,
                title: "t".into(),
                date: None,
                context: None,
                body,
                message_count: Some(count),
                want_details: false,
            }),
            native_summary: None,
        })))
    }
}

use sb_core::SyncOptions as SyncOptions_;

struct Factory(Arc<FakeSource>);

impl SourceFactory for Factory {
    fn source(
        &self,
        account: &Account,
        _c: &Catalog,
    ) -> Result<Option<Arc<dyn Source>>, PipelineError> {
        Ok((account.kind == AccountKind::Slack).then(|| self.0.clone() as Arc<dyn Source>))
    }
}

struct Env {
    _dir: tempfile::TempDir,
    home: Home,
    remote: Remote,
    fetches: Fetches,
}

fn long_msg(i: usize) -> String {
    format!(
        "10:{i:02} Alice: this is a reasonably long message number {i} about the CSV export plan"
    )
}

async fn env(llm: &MockServer) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let cat = Catalog::create(&home).unwrap();
    cat.ensure_account(&AccountId::new("acme").unwrap(), AccountKind::Slack, "Acme")
        .unwrap();
    cat.set_setting(
        "llm.profiles.fake",
        &json!({"kind": "local_llm", "provider": "openai_compatible", "model": "fake-model",
                "base_url": format!("{}/v1", llm.uri()), "concurrency": 2}),
    )
    .unwrap();
    cat.set_setting("summary.profile.default", &json!("fake"))
        .unwrap();
    cat.set_setting("pipeline.commit_batch", &json!(2)).unwrap();
    cat.set_setting("summary.min_chars", &json!(50)).unwrap();
    let mut remote = BTreeMap::new();
    for t in 0..5 {
        remote.insert(format!("C1:{t}.0"), (0..4).map(long_msg).collect());
    }
    Env {
        _dir: dir,
        home,
        remote: Arc::new(Mutex::new(remote)),
        fetches: Arc::new(Mutex::new(vec![])),
    }
}

fn pipeline(env: &Env, cancel_after: Option<usize>) -> Pipeline {
    let cat = Catalog::open(&env.home).unwrap();
    let token = tokio_util::sync::CancellationToken::new();
    let src = Arc::new(FakeSource {
        remote: env.remote.clone(),
        fetches: env.fetches.clone(),
        cancel_after: cancel_after.map(|n| (n, token.clone())),
        fetched: AtomicUsize::new(0),
    });
    let mut p = Pipeline::new(cat, Arc::new(Factory(src)));
    p.cancel = token;
    p
}

async fn mock_llm() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "{\"overview\":\"They planned the CSV export.\",\"decisions\":[\"Alice decided to use CSV\"],\"action_items\":[]}"}}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 20}
        })))
        .mount(&server)
        .await;
    server
}

/// Number of summarization requests (excluding the tiny warm-up calls).
async fn summary_calls(llm: &MockServer) -> usize {
    llm.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/v1/chat/completions")
        .filter(|r| !String::from_utf8_lossy(&r.body).contains("ping"))
        .count()
}

/// A comparable snapshot of the catalog.
fn snapshot(
    home: &Home,
) -> Vec<(
    String,
    String,
    String,
    Vec<(String, String)>,
    Option<String>,
)> {
    let cat = Catalog::open(home).unwrap();
    let mut out = Vec::new();
    for e in cat.list_entries(&EntryFilter::default()).unwrap() {
        let secs = cat
            .sections(e.id)
            .unwrap()
            .into_iter()
            .map(|s| (s.kind.to_string(), s.text))
            .collect();
        out.push((
            e.source_id.clone(),
            e.summary_status.to_string(),
            e.raw_hash.clone().unwrap_or_default(),
            secs,
            cat.summary(e.id).unwrap().map(|s| s.input_hash),
        ));
    }
    out.sort();
    out
}

#[tokio::test]
async fn sync_fetches_summarizes_and_skips_unchanged() {
    let llm = mock_llm().await;
    let env = env(&llm).await;
    let p = pipeline(&env, None);
    let r = p.sync(&SyncOptions::default()).await.unwrap();
    assert_eq!(r.status, Some(RunStatus::Ok), "{:?}", r.stats.errors);
    assert_eq!(r.stats.sources["acme/slack.thread"].new, 5);
    assert_eq!(r.stats.summaries.summarized, 5);
    assert_eq!(summary_calls(&llm).await, 5);

    // Search finds the generated decision.
    {
        let cat = p.catalog();
        let hits = sb_store::fts::search(
            cat.conn(),
            &sb_core::search::SearchQuery {
                terms: vec!["decided".into()],
                sections: vec![SectionKind::Decisions],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 5);
    }

    // A second run with no remote changes fetches and summarizes nothing.
    let r = p.sync(&SyncOptions::default()).await.unwrap();
    assert_eq!(r.stats.committed_entries(), 0);
    assert_eq!(r.stats.summaries.summarized, 0);
    assert_eq!(summary_calls(&llm).await, 5);
}

#[tokio::test]
async fn grown_thread_appends_one_segment_and_resummarizes_once() {
    let llm = mock_llm().await;
    let env = env(&llm).await;
    let p = pipeline(&env, None);
    p.sync(&SyncOptions::default()).await.unwrap();
    env.fetches.lock().unwrap().clear();
    env.remote
        .lock()
        .unwrap()
        .get_mut("C1:2.0")
        .unwrap()
        .push(long_msg(9));

    let r = p.sync(&SyncOptions::default()).await.unwrap();
    assert_eq!(r.stats.sources["acme/slack.thread"].updated, 1);
    // Only the grown thread was fetched, from its stored state.
    let fetches = env.fetches.lock().unwrap().clone();
    assert_eq!(
        fetches,
        vec![("C1:2.0".to_string(), Some(json!({"count": 4})))]
    );
    let cat = p.catalog();
    let e = cat
        .entry_by_key("acme", SourceKind::SlackThread, "C1:2.0")
        .unwrap()
        .unwrap();
    let raws = cat.raw_objects(e.id).unwrap();
    assert_eq!(raws.len(), 2, "one new segment");
    assert!(
        cat.sections(e.id)
            .unwrap()
            .iter()
            .any(|s| s.text.contains("number 9"))
    );
    drop(cat);
    assert_eq!(
        summary_calls(&llm).await,
        6,
        "the grown thread is summarized once more"
    );
}

#[tokio::test]
async fn interrupted_run_resumes_to_the_same_catalog() {
    let llm_a = mock_llm().await;
    let env_a = env(&llm_a).await;
    pipeline(&env_a, None)
        .sync(&SyncOptions::default())
        .await
        .unwrap();

    let llm_b = mock_llm().await;
    let env_b = env(&llm_b).await;
    // Cancel in the middle of the second chunk (commit_batch = 2).
    let r = pipeline(&env_b, Some(3))
        .sync(&SyncOptions::default())
        .await
        .unwrap();
    assert_eq!(r.status, Some(RunStatus::Interrupted));
    assert!(r.stats.committed_entries() >= 2);
    assert!(r.stats.queue_remaining > 0);
    let r = pipeline(&env_b, None)
        .sync(&SyncOptions::default())
        .await
        .unwrap();
    assert_eq!(r.status, Some(RunStatus::Ok), "{:?}", r.stats.errors);

    assert_eq!(snapshot(&env_a.home), snapshot(&env_b.home));
    // No summary was requested twice.
    assert_eq!(summary_calls(&llm_b).await, 5);
}

#[tokio::test]
async fn limits_stop_cleanly() {
    let llm = mock_llm().await;
    let env = env(&llm).await;
    let p = pipeline(&env, None);
    let r = p
        .sync(&SyncOptions {
            limits: Limits {
                max_summaries: Some(2),
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(r.status, Some(RunStatus::StoppedByLimit));
    assert_eq!(r.stats.summaries.summarized, 2);
    assert_eq!(r.stats.pending_summaries, 3);
}

#[tokio::test]
async fn estimate_and_resummarize_skip_current() {
    let llm = mock_llm().await;
    let env = env(&llm).await;
    let p = pipeline(&env, None);
    p.sync(&SyncOptions {
        no_summary: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let est = p
        .summarize_pending(&SummarizeOptions {
            filter: EntryFilter::default(),
            target: Target::Configured,
            limits: Limits::default(),
            retry_failed: false,
            force: false,
            estimate_only: true,
        })
        .await
        .unwrap();
    assert_eq!(est.estimate.unwrap().entries, 5);
    assert_eq!(summary_calls(&llm).await, 0);

    let opts = SummarizeOptions {
        filter: EntryFilter::default(),
        target: Target::Profile("fake".into()),
        limits: Limits::default(),
        retry_failed: false,
        force: false,
        estimate_only: false,
    };
    let r = p.resummarize(&opts).await.unwrap();
    assert_eq!(r.stats.summarized, 5);
    // Re-running resumes: everything is already at the target generator.
    let r = p.resummarize(&opts).await.unwrap();
    assert_eq!(r.stats.summarized, 0);
    assert_eq!(r.stats.already_current, 5);
    let r = p
        .resummarize(&SummarizeOptions {
            force: true,
            ..opts
        })
        .await
        .unwrap();
    assert_eq!(r.stats.summarized, 5);
}

#[tokio::test]
async fn reextract_and_refetch() {
    let llm = mock_llm().await;
    let env = env(&llm).await;
    let p = pipeline(&env, None);
    p.sync(&SyncOptions::default()).await.unwrap();
    let r = p
        .reextract(&EntryFilter::default(), &Limits::default())
        .await
        .unwrap();
    assert_eq!(r.updated, 5);
    // Re-extraction with unchanged raw data keeps the summaries.
    assert_eq!(summary_calls(&llm).await, 5);
    let cat = p.catalog();
    assert_eq!(
        cat.count_entries(&EntryFilter {
            summary_status: vec![SummaryStatus::Done],
            ..Default::default()
        })
        .unwrap(),
        5
    );
    drop(cat);
    let r = p
        .refetch(
            &EntryFilter {
                source_kinds: vec![SourceKind::SlackThread],
                limit: Some(1),
                ..Default::default()
            },
            &Limits::default(),
        )
        .await
        .unwrap();
    assert_eq!(r.updated, 1);
    let fetches = env.fetches.lock().unwrap().clone();
    assert_eq!(
        fetches.last().unwrap().1,
        None,
        "full refetch without state"
    );
}

#[tokio::test]
async fn import_bundle_is_idempotent_and_never_downgrades() {
    let llm = mock_llm().await;
    let env = env(&llm).await;
    let p = pipeline(&env, None);
    p.sync(&SyncOptions {
        no_summary: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let bundle = env._dir.path().join("bundle");
    std::fs::create_dir_all(bundle.join("raw")).unwrap();
    std::fs::write(
        bundle.join("manifest.json"),
        json!({"format": "second-brain-import/v1", "created_at": "2026-10-01T12:00:00Z",
               "producer": "test/1", "accounts": {"slack": "acme"}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(bundle.join("raw/t9.txt"), "10:00 Bob: imported raw\n").unwrap();
    let lines = [
        // New entry with raw data.
        json!({"source_kind": "slack.thread", "source_id": "C9:1.0", "title": "#old thread",
               "ingested_at": "2025-01-01T00:00:00Z",
               "sections": [{"kind": "overview", "origin": "generated", "text": "Old overview"}],
               "summary": {"generator_kind": "llm_api", "provider": "anthropic", "model": "claude-haiku-4-5"},
               "raw": [{"role": "primary", "path": "raw/t9.txt"}]}),
        // New entry without raw data.
        json!({"source_kind": "slack.thread", "source_id": "C9:2.0", "title": "#old 2",
               "metadata": {"import_ref": "x2"},
               "sections": [{"kind": "decisions", "origin": "generated", "text": "- keep CSV"}]}),
        // Existing entry with raw data: only import_ref and ingested_at are merged.
        json!({"source_kind": "slack.thread", "source_id": "C1:0.0", "title": "should not replace",
               "ingested_at": "2020-01-01T00:00:00Z", "metadata": {"import_ref": "x3"},
               "sections": [{"kind": "overview", "text": "should not replace"}]}),
        // Invalid lines.
        json!({"source_kind": "slack.thread", "source_id": "C9:3.0", "title": "no sections", "sections": []}),
        json!({"source_kind": "slack.thread", "source_id": "C9:4.0", "title": "bad raw",
               "sections": [{"kind": "overview", "text": "x"}], "raw": [{"role": "primary", "path": "../etc/passwd"}]}),
    ];
    let mut text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    text.push_str("not json\n");
    std::fs::write(bundle.join("entries.jsonl"), text).unwrap();

    let dry = p.import_bundle(&bundle, &[], true).unwrap();
    assert_eq!(dry.invalid, 3);
    assert_eq!(dry.created, 0);
    assert!(
        p.catalog()
            .entry_by_key("acme", SourceKind::SlackThread, "C9:1.0")
            .unwrap()
            .is_none()
    );

    let r = p.import_bundle(&bundle, &[], false).unwrap();
    assert_eq!(
        (r.created, r.merged_into_present, r.invalid, r.raw_missing),
        (2, 1, 3, 1)
    );
    let before = snapshot(&env.home);
    let r = p.import_bundle(&bundle, &[], false).unwrap();
    assert_eq!(r.created + r.updated, 0, "{r:?}");
    assert_eq!(snapshot(&env.home), before);

    let cat = p.catalog();
    let e1 = cat
        .entry_by_key("acme", SourceKind::SlackThread, "C9:1.0")
        .unwrap()
        .unwrap();
    assert_eq!(e1.raw_status, RawStatus::Present);
    assert_eq!(e1.origin, EntryOrigin::Import);
    assert_eq!(e1.summary_status, SummaryStatus::Done);
    assert_eq!(
        cat.summary(e1.id).unwrap().unwrap().model,
        "claude-haiku-4-5"
    );
    let e2 = cat
        .entry_by_key("acme", SourceKind::SlackThread, "C9:2.0")
        .unwrap()
        .unwrap();
    assert_eq!(e2.raw_status, RawStatus::Missing);
    assert_eq!(
        cat.summary(e2.id).unwrap().unwrap().generator_kind,
        GeneratorKind::Unknown
    );
    let e3 = cat
        .entry_by_key("acme", SourceKind::SlackThread, "C1:0.0")
        .unwrap()
        .unwrap();
    assert_eq!(e3.metadata["import_ref"], "x3");
    assert!(e3.title.starts_with("#dev"));
    assert_eq!(util::ts(e3.ingested_at), "2020-01-01T00:00:00Z");
}

// ---------------------------------------------------------------------------
// Budget (ADR-0013)
// ---------------------------------------------------------------------------

use sb_core::clock::FixedClock;

/// Wednesday of the week starting Monday 2026-10-05.
const NOW: &str = "2026-10-07T10:00:00Z";

/// A mock Anthropic API answering every request with `text` and fixed usage.
async fn mock_anthropic(text: &str, input: u64, output: u64) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": input, "output_tokens": output}
        })))
        .mount(&server)
        .await;
    server
}

const GOOD: &str = r#"{"overview":"They planned the export.","decisions":[],"action_items":[]}"#;

async fn api_calls(llm: &MockServer) -> usize {
    llm.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/v1/messages")
        .count()
}

/// Summarize with a paid Anthropic profile (`concurrency` 1, so runs are
/// deterministic) and set the caps. Times are UTC.
fn use_api_profile(env: &Env, llm: &MockServer, model: &str, weekly: Value, monthly: Value) {
    let cat = Catalog::open(&env.home).unwrap();
    cat.set_setting(
        "llm.profiles.claude",
        &json!({"kind": "llm_api", "provider": "anthropic", "model": model,
                "base_url": llm.uri(), "concurrency": 1}),
    )
    .unwrap();
    cat.set_secret(
        &sb_store::SecretScope::Global,
        "anthropic.api_key",
        &sb_core::Secret::new("k"),
    )
    .unwrap();
    cat.set_setting("summary.profile.default", &json!("claude"))
        .unwrap();
    cat.set_setting("summary.budget.timezone", &json!("UTC"))
        .unwrap();
    cat.set_setting("summary.budget.weekly_usd", &weekly)
        .unwrap();
    cat.set_setting("summary.budget.monthly_usd", &monthly)
        .unwrap();
}

fn pipeline_at(env: &Env, clock: &Arc<FixedClock>) -> Pipeline {
    let cat = Catalog::open_with_clock(&env.home, clock.clone()).unwrap();
    let src = Arc::new(FakeSource {
        remote: env.remote.clone(),
        fetches: env.fetches.clone(),
        cancel_after: None,
        fetched: AtomicUsize::new(0),
    });
    Pipeline::new(cat, Arc::new(Factory(src)))
}

fn open_issue_codes(env: &Env) -> Vec<String> {
    Catalog::open(&env.home)
        .unwrap()
        .open_issues()
        .unwrap()
        .into_iter()
        .map(|i| i.code)
        .collect()
}

fn fixed_clock(t: &str) -> Arc<FixedClock> {
    Arc::new(FixedClock::new(util::parse_ts(t).unwrap()))
}

#[tokio::test]
async fn budget_stops_the_stage_then_resumes_when_the_cap_is_raised() {
    // Each call costs $0.2 (100k tokens in at $1/M, 20k out at $5/M).
    let llm = mock_anthropic(GOOD, 100_000, 20_000).await;
    let env = env(&mock_llm().await).await;
    use_api_profile(&env, &llm, "claude-haiku-4-5", json!(0.5), json!(100.0));
    let clock = fixed_clock(NOW);

    let r = pipeline_at(&env, &clock)
        .sync(&SyncOptions::default())
        .await
        .unwrap();
    assert_eq!(r.status, Some(RunStatus::StoppedByLimit));
    assert_eq!(
        r.stats.stop,
        Some(sb_pipeline::Stop::Limit("budget.weekly".into()))
    );
    // $0.2 + $0.2 + $0.2 > $0.5: the fourth unit is not started.
    assert_eq!(r.stats.summaries.summarized, 3);
    assert_eq!(r.stats.pending_summaries, 2);
    assert_eq!(api_calls(&llm).await, 3);
    assert!(open_issue_codes(&env).contains(&"llm.budget_exhausted".to_string()));

    // The ledger and the period snapshot.
    let cat = Catalog::open_with_clock(&env.home, clock.clone()).unwrap();
    let (spend, _) = cat.spend_total().unwrap();
    assert!((spend.cost_usd - 0.6).abs() < 1e-9, "{spend:?}");
    assert_eq!(spend.calls, 3);
    let week = cat
        .period_rows(sb_core::budget::PeriodKind::Week, 5)
        .unwrap();
    assert_eq!(week.len(), 1);
    assert_eq!(week[0].cap_usd, Some(0.5));
    assert!(week[0].stopped_at.is_some());
    // Every ledger row belongs to the sync run.
    let run_ids: Vec<Option<i64>> = cat
        .conn()
        .prepare("SELECT DISTINCT run_id FROM llm_usage")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(run_ids, vec![r.run_id]);
    drop(cat);

    // Still blocked: a second run in the same week starts nothing.
    let r = pipeline_at(&env, &clock)
        .sync(&SyncOptions::default())
        .await
        .unwrap();
    assert_eq!(r.stats.summaries.summarized, 0);
    assert_eq!(api_calls(&llm).await, 3);

    // Raising the cap is enough to resume; the issue is resolved.
    Catalog::open(&env.home)
        .unwrap()
        .set_setting("summary.budget.weekly_usd", &json!(5.0))
        .unwrap();
    let r = pipeline_at(&env, &clock)
        .sync(&SyncOptions::default())
        .await
        .unwrap();
    assert_eq!(r.status, Some(RunStatus::Ok), "{:?}", r.stats.errors);
    assert_eq!(r.stats.summaries.summarized, 2);
    assert_eq!(api_calls(&llm).await, 5);
    assert!(!open_issue_codes(&env).contains(&"llm.budget_exhausted".to_string()));
}

#[tokio::test]
async fn budget_resumes_after_the_week_rolls_over_but_the_month_still_binds() {
    let llm = mock_anthropic(GOOD, 100_000, 20_000).await;
    let env = env(&mock_llm().await).await;
    // $0.5 per week, $0.9 per month: three calls fit in the month in total.
    use_api_profile(&env, &llm, "claude-haiku-4-5", json!(0.5), json!(0.9));
    let clock = fixed_clock(NOW);

    let r = pipeline_at(&env, &clock)
        .sync(&SyncOptions::default())
        .await
        .unwrap();
    assert_eq!(r.stats.summaries.summarized, 3, "the weekly cap stops it");
    assert_eq!(
        r.stats.stop,
        Some(sb_pipeline::Stop::Limit("budget.weekly".into()))
    );

    // Monday: the weekly cap has room again, but $0.6 of $0.9 is already spent
    // this month, so one more call is estimated to fit and then the month binds.
    clock.set(util::parse_ts("2026-10-12T00:00:00Z").unwrap());
    let r = pipeline_at(&env, &clock)
        .sync(&SyncOptions::default())
        .await
        .unwrap();
    assert_eq!(r.stats.summaries.summarized, 2);
    assert_eq!(r.status, Some(RunStatus::Ok), "{:?}", r.stats.errors);

    let cat = Catalog::open_with_clock(&env.home, clock.clone()).unwrap();
    assert_eq!(
        cat.period_rows(sb_core::budget::PeriodKind::Week, 5)
            .unwrap()
            .len(),
        2,
        "one snapshot per week the tool evaluated"
    );
    let (spend, _) = cat.spend_total().unwrap();
    assert!((spend.cost_usd - 1.0).abs() < 1e-9, "{spend:?}");
}

#[tokio::test]
async fn an_unpriced_paid_profile_is_refused_while_a_cap_is_enabled() {
    let llm = mock_anthropic(GOOD, 100, 10).await;
    let env = env(&mock_llm().await).await;
    use_api_profile(&env, &llm, "mystery-model", json!(2.0), json!(10.0));
    let clock = fixed_clock(NOW);

    let r = pipeline_at(&env, &clock)
        .sync(&SyncOptions::default())
        .await
        .unwrap();
    assert_eq!(r.stats.summaries.summarized, 0);
    assert_eq!(api_calls(&llm).await, 0);
    assert!(open_issue_codes(&env).contains(&"llm.unpriced".to_string()));
    assert_eq!(r.stats.pending_summaries, 5, "entries stay pending");

    // Disabling both caps explicitly lets it run, and resolves the issue.
    Catalog::open(&env.home)
        .unwrap()
        .set_setting("summary.budget.weekly_usd", &json!(0))
        .unwrap();
    Catalog::open(&env.home)
        .unwrap()
        .set_setting("summary.budget.monthly_usd", &Value::Null)
        .unwrap();
    let r = pipeline_at(&env, &clock)
        .sync(&SyncOptions::default())
        .await
        .unwrap();
    assert_eq!(r.stats.summaries.summarized, 5);
    assert!(!open_issue_codes(&env).contains(&"llm.unpriced".to_string()));
    // Unknown cost: the calls are in the ledger, counted as unpriced.
    let (spend, _) = Catalog::open(&env.home).unwrap().spend_total().unwrap();
    assert_eq!((spend.cost_usd, spend.unpriced_calls), (0.0, 5));
}

#[tokio::test]
async fn local_models_are_never_blocked_and_are_recorded_at_zero_cost() {
    let llm = mock_llm().await;
    let env = env(&llm).await;
    {
        let cat = Catalog::open(&env.home).unwrap();
        // A tiny cap that would stop any paid profile at once.
        cat.set_setting("summary.budget.weekly_usd", &json!(0.0001))
            .unwrap();
    }
    let r = pipeline(&env, None)
        .sync(&SyncOptions::default())
        .await
        .unwrap();
    assert_eq!(r.status, Some(RunStatus::Ok), "{:?}", r.stats.errors);
    assert_eq!(r.stats.summaries.summarized, 5);
    let (spend, _) = Catalog::open(&env.home).unwrap().spend_total().unwrap();
    assert_eq!(
        (spend.cost_usd, spend.calls, spend.unpriced_calls),
        (0.0, 5, 0)
    );
}

#[tokio::test]
async fn a_billed_failed_attempt_is_recorded_in_the_ledger() {
    // Invalid JSON every time: three generations with a repair each = 6 calls.
    let llm = mock_anthropic("not json", 1_000, 100).await;
    let env = env(&mock_llm().await).await;
    use_api_profile(&env, &llm, "claude-haiku-4-5", json!(2.0), json!(10.0));
    let clock = fixed_clock(NOW);

    let r = pipeline_at(&env, &clock)
        .sync(&SyncOptions {
            limits: Limits {
                max_summaries: Some(1),
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(r.stats.summaries.failed, 1);
    assert_eq!(api_calls(&llm).await, 6);
    // 6 x (1000 in x $1/M + 100 out x $5/M) = $0.009
    let cat = Catalog::open(&env.home).unwrap();
    let (spend, _) = cat.spend_total().unwrap();
    assert_eq!(spend.calls, 6);
    assert!((spend.cost_usd - 0.009).abs() < 1e-9, "{spend:?}");
    let outcome: String = cat
        .conn()
        .query_row("SELECT outcome FROM llm_usage", [], |r| r.get(0))
        .unwrap();
    assert_eq!(outcome, "failed");
    // It also shows in the run's own cost figure.
    assert!((r.stats.summaries.cost_usd - 0.009).abs() < 1e-9);
}

#[tokio::test]
async fn a_raised_min_chars_skips_pending_entries_without_an_llm_call() {
    let llm = mock_llm().await;
    let env = env(&llm).await;
    let p = pipeline(&env, None);
    p.sync(&SyncOptions {
        no_summary: true,
        ..Default::default()
    })
    .await
    .unwrap();
    Catalog::open(&env.home)
        .unwrap()
        .set_setting("summary.min_chars", &json!(100_000))
        .unwrap();
    let r = p
        .summarize_pending(&SummarizeOptions {
            filter: EntryFilter::default(),
            target: Target::Configured,
            limits: Limits::default(),
            retry_failed: false,
            force: false,
            estimate_only: false,
        })
        .await
        .unwrap();
    assert_eq!(r.stats.summarized, 0);
    assert_eq!(r.stats.skipped, 5);
    assert_eq!(summary_calls(&llm).await, 0);
    let cat = p.catalog();
    assert_eq!(
        cat.count_entries(&EntryFilter {
            summary_status: vec![SummaryStatus::Skipped],
            ..Default::default()
        })
        .unwrap(),
        5
    );
}

#[tokio::test]
async fn an_existing_summary_survives_a_raised_threshold() {
    let llm = mock_llm().await;
    let env = env(&llm).await;
    pipeline(&env, None)
        .sync(&SyncOptions::default())
        .await
        .unwrap();
    Catalog::open(&env.home)
        .unwrap()
        .set_setting("summary.min_chars", &json!(100_000))
        .unwrap();
    let r = pipeline(&env, None)
        .sync(&SyncOptions::default())
        .await
        .unwrap();
    assert_eq!(r.stats.summaries.summarized, 0);
    let cat = Catalog::open(&env.home).unwrap();
    assert_eq!(
        cat.count_entries(&EntryFilter {
            summary_status: vec![SummaryStatus::Done],
            ..Default::default()
        })
        .unwrap(),
        5
    );
}

#[tokio::test]
async fn estimate_compares_with_the_remaining_budget() {
    let llm = mock_anthropic(GOOD, 100, 10).await;
    let env = env(&mock_llm().await).await;
    let clock = fixed_clock(NOW);
    let opts = SummarizeOptions {
        filter: EntryFilter::default(),
        target: Target::Configured,
        limits: Limits::default(),
        retry_failed: false,
        force: false,
        estimate_only: true,
    };
    // Each of the five threads is estimated at about $0.0036.
    use_api_profile(&env, &llm, "claude-haiku-4-5", json!(0.01), json!(100.0));
    let p = pipeline_at(&env, &clock);
    p.sync(&SyncOptions {
        no_summary: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let est = p.summarize_pending(&opts).await.unwrap().estimate.unwrap();
    let b = est
        .budget
        .expect("a cap is enabled and the profile is paid");
    assert_eq!(b.paid_entries, 5);
    assert_eq!(b.entries_that_fit, 2, "{b:?}");
    assert!(!b.fits);
    assert!((b.remaining_usd - 0.01).abs() < 1e-9);
    assert_eq!(api_calls(&llm).await, 0, "an estimate makes no call");

    // A cap that is disabled gives no budget block.
    Catalog::open(&env.home)
        .unwrap()
        .set_setting("summary.budget.weekly_usd", &json!(0))
        .unwrap();
    Catalog::open(&env.home)
        .unwrap()
        .set_setting("summary.budget.monthly_usd", &json!(0))
        .unwrap();
    let est = pipeline_at(&env, &clock)
        .summarize_pending(&opts)
        .await
        .unwrap()
        .estimate
        .unwrap();
    assert!(est.budget.is_none());
}
