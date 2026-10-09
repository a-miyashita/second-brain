//! Slack sync through the pipeline against a mock Slack Web API.
//! All names, IDs and messages are synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)]
// Guards are dropped explicitly before awaiting; clippy cannot see `drop`.
#![allow(clippy::await_holding_lock)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use sb_pipeline::sync::SyncOptions;
use sb_pipeline::{Pipeline, PipelineError, SourceFactory};
use sb_slack::SlackSource;
use sb_store::{Account, Catalog, Home};
use second_brain_kernel::clock::FixedClock;
use second_brain_kernel::source::Source;
use second_brain_kernel::{AccountId, AccountKind, RawStatus, Secret, SourceKind, SummaryStatus};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Mock workspace state: channel → messages (parents and replies).
type Messages = Arc<Mutex<BTreeMap<String, Vec<Value>>>>;

fn q(req: &Request, key: &str) -> Option<String> {
    req.url
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.to_string())
}

fn f(ts: &str) -> f64 {
    ts.parse().unwrap()
}

fn msg(ts: &str, user: &str, text: &str) -> Value {
    json!({"type": "message", "ts": ts, "user": user, "text": text})
}

/// Called for every `conversations.history` request (before it is answered).
type Hook = Arc<dyn Fn(&Request) + Send + Sync>;

async fn slack_mock(state: Messages) -> MockServer {
    slack_mock_hooked(state, None).await
}

async fn slack_mock_hooked(state: Messages, hook: Option<Hook>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(path("/users.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true, "members": [
            {"id": "U1", "name": "alice", "real_name": "Alice Example", "profile": {"real_name": "Alice Example"}},
            {"id": "U2", "name": "bob", "real_name": "Bob Example", "profile": {"real_name": "Bob Example"}}
        ]})))
        .mount(&server)
        .await;
    Mock::given(path("/users.conversations"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"ok": true, "channels": [
                {"id": "C1", "name": "dev", "is_channel": true, "is_member": true},
                {"id": "C2", "name": "random", "is_channel": true, "is_member": true},
                {"id": "D1", "is_im": true, "user": "U2"}
            ]})),
        )
        .mount(&server)
        .await;
    let st = state.clone();
    Mock::given(path("/conversations.history"))
        .respond_with(move |req: &Request| {
            if let Some(h) = &hook {
                h(req);
            }
            let ch = q(req, "channel").unwrap();
            let oldest = f(&q(req, "oldest").unwrap());
            let latest = f(&q(req, "latest").unwrap());
            let all = st.lock().unwrap().get(&ch).cloned().unwrap_or_default();
            // Top-level messages only, with thread summary fields on parents.
            let msgs: Vec<Value> = all
                .iter()
                .filter(|m| m.get("thread_ts").is_none_or(|t| t == &m["ts"]))
                .filter(|m| {
                    let t = f(m["ts"].as_str().unwrap());
                    t >= oldest && t <= latest
                })
                .map(|m| {
                    let ts = m["ts"].as_str().unwrap();
                    let replies: Vec<&Value> = all
                        .iter()
                        .filter(|r| {
                            r.get("thread_ts").and_then(Value::as_str) == Some(ts)
                                && r["ts"] != m["ts"]
                        })
                        .collect();
                    let mut m = m.clone();
                    if !replies.is_empty() {
                        m["thread_ts"] = json!(ts);
                        m["reply_count"] = json!(replies.len());
                        m["latest_reply"] = replies.last().unwrap()["ts"].clone();
                    }
                    m
                })
                .rev()
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({"ok": true, "messages": msgs}))
        })
        .mount(&server)
        .await;
    let st = state.clone();
    Mock::given(path("/conversations.replies"))
        .respond_with(move |req: &Request| {
            let ch = q(req, "channel").unwrap();
            let ts = q(req, "ts").unwrap();
            let oldest = q(req, "oldest").map(|o| f(&o)).unwrap_or(0.0);
            let all = st.lock().unwrap().get(&ch).cloned().unwrap_or_default();
            // Slack returns the parent even when `oldest` excludes it.
            let msgs: Vec<Value> = all
                .iter()
                .filter(|m| {
                    m["ts"] == ts
                        || (m.get("thread_ts").and_then(Value::as_str) == Some(&ts)
                            && f(m["ts"].as_str().unwrap()) > oldest)
                })
                .cloned()
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({"ok": true, "messages": msgs}))
        })
        .mount(&server)
        .await;
    Mock::given(path("/search.messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"ok": true, "messages": {"matches": [], "paging": {"pages": 1}}}),
        ))
        .mount(&server)
        .await;
    server
}

async fn llm_mock() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "{\"overview\":\"Discussed the release.\",\"decisions\":[],\"action_items\":[]}"}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        })))
        .mount(&server)
        .await;
    server
}

struct Factory(String);

impl SourceFactory for Factory {
    fn source(
        &self,
        account: &Account,
        _c: &Catalog,
    ) -> Result<Option<Arc<dyn Source>>, PipelineError> {
        let s = SlackSource::new(
            account.ctx(),
            Some(Secret::new("xoxp-test")),
            Some(self.0.clone()),
            "UTC".into(),
        )?;
        Ok(Some(Arc::new(s)))
    }
}

fn pipeline(home: &Home, slack: &MockServer) -> Pipeline {
    let clock = Arc::new(FixedClock::new(
        second_brain_kernel::util::parse_ts("2026-09-10T00:00:00Z").unwrap(),
    ));
    let cat = Catalog::open_with_clock(home, clock).unwrap();
    Pipeline::new(cat, Arc::new(Factory(slack.uri())))
}

// 2026-09-08T00:00:00Z = 1788825600.
const T0: i64 = 1_788_825_600;

fn ts(offset: i64) -> String {
    format!("{}.000100", T0 + offset)
}

fn long(i: usize) -> String {
    format!(
        "Reply {i}: the release checklist needs one more review before Friday, please take a look"
    )
}

#[tokio::test]
async fn incremental_thread_and_day_entries() {
    let state: Messages = Arc::new(Mutex::new(BTreeMap::new()));
    {
        let mut s = state.lock().unwrap();
        let parent = ts(0);
        let mut c1 = vec![msg(&parent, "U2", "Release plan for v2 <@U1>")];
        for i in 1..=3 {
            let mut r = msg(
                &ts(i * 60),
                if i % 2 == 0 { "U2" } else { "U1" },
                &long(i as usize),
            );
            r["thread_ts"] = json!(parent);
            c1.push(r);
        }
        c1.push(msg(&ts(3600), "U1", "lunch?"));
        s.insert("C1".into(), c1);
        // In #random (involvement only) only threads involving me count.
        s.insert(
            "C2".into(),
            vec![
                msg(&ts(100), "U2", "unrelated chatter"),
                msg(&ts(200), "U2", "<!here> office closed on Friday"),
            ],
        );
        s.insert(
            "D1".into(),
            vec![
                msg(&ts(7200), "U2", "hi there"),
                msg(&ts(7260), "U1", "hello"),
            ],
        );
    }
    let slack = slack_mock(state.clone()).await;
    let llm = llm_mock().await;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    {
        let cat = Catalog::create(&home).unwrap();
        let mut config = sb_slack::default_config_json();
        config["full_channels"] = json!(["dev"]);
        config["team_url"] = json!("https://acme.slack.test/");
        cat.add_account(
            &AccountId::new("acme").unwrap(),
            AccountKind::Slack,
            "Acme",
            Some("T1:U1"),
            &config,
        )
        .unwrap();
        cat.set_setting(
            "llm.profiles.fake",
            &json!({"kind": "local_llm", "provider": "openai_compatible", "model": "m", "base_url": format!("{}/v1", llm.uri())}),
        )
        .unwrap();
        cat.set_setting("summary.profile.default", &json!("fake"))
            .unwrap();
        cat.set_setting("slack.day_timezone", &json!("UTC"))
            .unwrap();
        cat.set_setting("summary.min_chars", &json!(200)).unwrap();
    }

    let p = pipeline(&home, &slack);
    let r = p.sync(&SyncOptions::default()).await.unwrap();
    assert!(r.stats.errors.is_empty(), "{:?}", r.stats.errors);
    {
        let cat = p.catalog();
        let thread = cat
            .entry_by_key("acme", SourceKind::SlackThread, &format!("C1:{}", ts(0)))
            .unwrap()
            .expect("thread entry");
        assert_eq!(thread.raw_status, RawStatus::Present);
        assert_eq!(thread.summary_status, SummaryStatus::Done);
        assert_eq!(thread.title, "#dev Release plan for v2");
        assert_eq!(
            thread.source_url.as_deref(),
            Some(
                format!(
                    "https://acme.slack.test/archives/C1/p{}",
                    ts(0).replace('.', "")
                )
                .as_str()
            )
        );
        assert_eq!(
            thread.fetch_state.as_ref().unwrap()["last_ts"],
            json!(ts(180))
        );
        // Non-thread messages of the full channel and the DM become day entries.
        let day = cat
            .entry_by_key("acme", SourceKind::SlackDay, "C1:day:2026-09-08")
            .unwrap()
            .unwrap();
        assert!(
            cat.sections(day.id).unwrap()[0]
                .text
                .contains("Alice Example: lunch?")
        );
        assert_eq!(
            day.summary_status,
            SummaryStatus::Skipped,
            "below thresholds"
        );
        let dm = cat
            .entry_by_key("acme", SourceKind::SlackDay, "D1:day:2026-09-08")
            .unwrap()
            .unwrap();
        assert_eq!(dm.title, "DM: Bob Example 2026-09-08 conversation");
        // #random: only the @here parent is ingested, as a one-message thread.
        assert!(
            cat.entry_by_key("acme", SourceKind::SlackThread, &format!("C2:{}", ts(200)))
                .unwrap()
                .is_some()
        );
        assert!(
            cat.entry_by_key("acme", SourceKind::SlackThread, &format!("C2:{}", ts(100)))
                .unwrap()
                .is_none()
        );
        assert!(
            cat.entry_by_key("acme", SourceKind::SlackDay, "C2:day:2026-09-08")
                .unwrap()
                .is_none()
        );
    }
    let summaries_before = count_summaries(&llm).await;

    // The thread gains a reply.
    {
        let mut s = state.lock().unwrap();
        let mut r = msg(&ts(4000), "U2", &long(4));
        r["thread_ts"] = json!(ts(0));
        s.get_mut("C1").unwrap().push(r);
    }
    slack.reset().await;
    let slack2 = slack_mock(state.clone()).await;
    let p = pipeline(&home, &slack2);
    let r = p.sync(&SyncOptions::default()).await.unwrap();
    assert!(r.stats.errors.is_empty(), "{:?}", r.stats.errors);
    // The replies request asked only for messages after the stored last_ts.
    let replies: Vec<Request> = slack2
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| {
            r.url.path() == "/conversations.replies" && q(r, "channel").as_deref() == Some("C1")
        })
        .collect();
    assert_eq!(replies.len(), 1);
    assert_eq!(q(&replies[0], "oldest").as_deref(), Some(ts(180).as_str()));
    let cat = p.catalog();
    let thread = cat
        .entry_by_key("acme", SourceKind::SlackThread, &format!("C1:{}", ts(0)))
        .unwrap()
        .unwrap();
    assert_eq!(
        cat.raw_objects(thread.id).unwrap().len(),
        2,
        "one new segment"
    );
    let details = &cat
        .sections(thread.id)
        .unwrap()
        .into_iter()
        .find(|s| s.kind == second_brain_kernel::SectionKind::Details)
        .unwrap()
        .text;
    assert_eq!(
        details.matches("Release plan for v2").count(),
        1,
        "parent de-duplicated"
    );
    assert!(details.contains("Reply 4"));
    drop(cat);
    assert_eq!(
        count_summaries(&llm).await,
        summaries_before + 1,
        "re-summarized once"
    );
}

async fn count_summaries(llm: &MockServer) -> usize {
    llm.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| {
            r.url.path() == "/v1/chat/completions"
                && !String::from_utf8_lossy(&r.body).contains("ping")
        })
        .count()
}

// ---------- initial window and backward coverage (ADR-0016) ----------

// 2026-09-10T00:00:00Z, the fixed clock of `pipeline`.
const NOW: i64 = 1_788_998_400;

fn day_ts(days_ago: i64) -> String {
    // 01:00 UTC of the day `days_ago` days before NOW.
    format!("{}.000100", NOW - days_ago * 86_400 + 3600)
}

fn day_id(days_ago: i64) -> String {
    let d = chrono::DateTime::from_timestamp(NOW - days_ago * 86_400, 0).unwrap();
    format!("C1:day:{}", d.format("%Y-%m-%d"))
}

fn window_home(slack_state: &Messages) -> (tempfile::TempDir, Home) {
    {
        let mut s = slack_state.lock().unwrap();
        s.insert(
            "C1".into(),
            [5, 20, 45, 80]
                .iter()
                .map(|d| msg(&day_ts(*d), "U2", &format!("note from {d} days ago")))
                .collect(),
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let cat = Catalog::create(&home).unwrap();
    let mut config = sb_slack::default_config_json();
    config["full_channels"] = json!(["dev"]);
    config["include_dms"] = json!(false);
    config["team_url"] = json!("https://acme.slack.test/");
    cat.add_account(
        &AccountId::new("acme").unwrap(),
        AccountKind::Slack,
        "Acme",
        Some("T1:U1"),
        &config,
    )
    .unwrap();
    cat.set_setting("slack.day_timezone", &json!("UTC"))
        .unwrap();
    (dir, home)
}

fn has(p: &Pipeline, days_ago: i64) -> bool {
    p.catalog()
        .entry_by_key("acme", SourceKind::SlackDay, &day_id(days_ago))
        .unwrap()
        .is_some()
}

fn covered_since(p: &Pipeline) -> String {
    p.catalog()
        .cursor(
            &AccountId::new("acme").unwrap(),
            SourceKind::SlackThread,
            "conv:C1",
        )
        .unwrap()
        .and_then(|c| c["covered_since"].as_str().map(str::to_string))
        .unwrap_or_default()
}

fn midnight(days_ago: i64) -> String {
    chrono::DateTime::from_timestamp(NOW - days_ago * 86_400, 0)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn history_oldest(reqs: &[Request]) -> Vec<f64> {
    reqs.iter()
        .filter(|r| {
            r.url.path() == "/conversations.history" && q(r, "channel").as_deref() == Some("C1")
        })
        .map(|r| f(&q(r, "oldest").unwrap()))
        .collect()
}

fn sync_opts(since_days: Option<i64>, until_days: Option<i64>) -> SyncOptions {
    let at = |d: i64| second_brain_kernel::util::parse_ts(&midnight(d)).unwrap();
    SyncOptions {
        no_summary: true,
        since: since_days.map(at),
        until: until_days.map(at),
        ..Default::default()
    }
}

#[tokio::test]
async fn first_sync_covers_the_initial_window_and_since_extends_it() {
    let state: Messages = Arc::new(Mutex::new(BTreeMap::new()));
    let (_dir, home) = window_home(&state);
    let slack = slack_mock(state.clone()).await;
    let p = pipeline(&home, &slack);

    // First run: 30 days.
    let r = p.sync(&sync_opts(None, None)).await.unwrap();
    assert!(r.stats.errors.is_empty(), "{:?}", r.stats.errors);
    assert!(has(&p, 5) && has(&p, 20));
    assert!(!has(&p, 45) && !has(&p, 80));
    assert_eq!(covered_since(&p), midnight(30));

    // Extend to 60 days: only 31..60 days are requested.
    let before = slack.received_requests().await.unwrap().len();
    let r = p.sync(&sync_opts(Some(60), None)).await.unwrap();
    assert!(r.stats.errors.is_empty(), "{:?}", r.stats.errors);
    assert!(has(&p, 45) && !has(&p, 80));
    assert_eq!(covered_since(&p), midnight(60));
    let reqs = slack.received_requests().await.unwrap()[before..].to_vec();
    let low = (NOW - 60 * 86_400) as f64;
    let high = (NOW - 30 * 86_400) as f64;
    for o in history_oldest(&reqs) {
        // The forward step asks from the cursor (recent); the backward step
        // never reaches into the covered 30 days.
        assert!(
            o >= high || (low..high).contains(&o),
            "unexpected oldest {o}"
        );
    }
    assert!(
        history_oldest(&reqs)
            .iter()
            .filter(|o| **o < high)
            .all(|o| *o >= low),
        "the backward step stays inside 31..60 days"
    );

    // `--since` inside the covered interval fetches nothing backwards.
    let before = slack.received_requests().await.unwrap().len();
    p.sync(&sync_opts(Some(50), None)).await.unwrap();
    let reqs = slack.received_requests().await.unwrap()[before..].to_vec();
    assert!(history_oldest(&reqs).iter().all(|o| *o >= high));
    assert_eq!(covered_since(&p), midnight(60));

    // Further back.
    p.sync(&sync_opts(Some(90), None)).await.unwrap();
    assert!(has(&p, 80));
    assert_eq!(covered_since(&p), midnight(90));
}

#[tokio::test]
async fn detached_explicit_window_is_fetched_but_not_recorded() {
    let state: Messages = Arc::new(Mutex::new(BTreeMap::new()));
    let (_dir, home) = window_home(&state);
    let slack = slack_mock(state.clone()).await;
    let p = pipeline(&home, &slack);
    p.sync(&sync_opts(None, None)).await.unwrap();
    assert_eq!(covered_since(&p), midnight(30));

    p.sync(&sync_opts(Some(85), Some(75))).await.unwrap();
    assert!(has(&p, 80), "the window is fetched");
    assert!(!has(&p, 45), "the gap in between is not");
    assert_eq!(covered_since(&p), midnight(30), "and not recorded");
}

#[tokio::test]
async fn cursor_without_covered_since_is_treated_as_unknown() {
    let state: Messages = Arc::new(Mutex::new(BTreeMap::new()));
    let (_dir, home) = window_home(&state);
    let slack = slack_mock(state.clone()).await;
    let p = pipeline(&home, &slack);
    p.sync(&sync_opts(None, None)).await.unwrap();
    // A cursor written before ADR-0016.
    p.catalog()
        .commit_batch(&sb_store::CommitBatch {
            account_id: Some(AccountId::new("acme").unwrap()),
            cursors: vec![second_brain_kernel::CursorUpdate {
                source_kind: SourceKind::SlackThread,
                key: "conv:C1".into(),
                value: Some(json!({"oldest": format!("{NOW}.000000"), "last_activity": day_ts(5)})),
            }],
            ..Default::default()
        })
        .unwrap();
    p.sync(&sync_opts(Some(60), None)).await.unwrap();
    assert!(has(&p, 45));
    assert_eq!(covered_since(&p), midnight(60));
}

#[tokio::test]
async fn interrupted_extension_keeps_its_windows_and_resumes() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let state: Messages = Arc::new(Mutex::new(BTreeMap::new()));
    let (_dir, home) = window_home(&state);
    let token = tokio_util::sync::CancellationToken::new();
    let armed = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let hook: Hook = {
        let (token, armed, calls) = (token.clone(), armed.clone(), calls.clone());
        Arc::new(move |_| {
            if armed.load(Ordering::SeqCst) && calls.fetch_add(1, Ordering::SeqCst) + 1 == 3 {
                token.cancel();
            }
        })
    };
    let slack = slack_mock_hooked(state.clone(), Some(hook)).await;
    let mut p = pipeline(&home, &slack);
    p.cancel = token;
    p.sync(&sync_opts(None, None)).await.unwrap();
    assert_eq!(covered_since(&p), midnight(30));

    // Stop after the second backward window (the first call is the forward step).
    armed.store(true, Ordering::SeqCst);
    p.sync(&sync_opts(Some(90), None)).await.unwrap();
    let partial = covered_since(&p);
    assert!(
        partial.as_str() < midnight(30).as_str() && partial.as_str() > midnight(90).as_str(),
        "partial coverage {partial}"
    );
    assert!(!has(&p, 80));

    // Resume: the same command continues from the recorded start.
    drop(p);
    let p = pipeline(&home, &slack);
    p.sync(&sync_opts(Some(90), None)).await.unwrap();
    assert_eq!(covered_since(&p), midnight(90));
    assert!(has(&p, 45) && has(&p, 80));
}
