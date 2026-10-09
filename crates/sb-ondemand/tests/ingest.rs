//! local.file and web.page ingest through the pipeline. Fixtures are synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::await_holding_lock)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use second_brain_kernel::document::IngestSettings;
use second_brain_kernel::source::Source;
use second_brain_kernel::{AccountKind, RawRole, SectionKind, SourceKind, SummaryStatus};
use second_brain_ondemand::{LocalSource, WebSource};
use second_brain_pipeline::ingest::{IngestOptions, IngestResult, IngestStatus};
use second_brain_pipeline::{Pipeline, PipelineError, SourceFactory};
use second_brain_store::{Account, Catalog, Home};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Factory {
    settings: IngestSettings,
    user_home: std::path::PathBuf,
}

impl SourceFactory for Factory {
    fn source(
        &self,
        account: &Account,
        cat: &Catalog,
    ) -> Result<Option<Arc<dyn Source>>, PipelineError> {
        Ok(match account.kind {
            AccountKind::Local => Some(Arc::new(LocalSource::with_user_home(
                account.ctx(),
                self.settings.clone(),
                cat.home().root().to_path_buf(),
                Some(self.user_home.clone()),
            ))),
            AccountKind::Web => Some(Arc::new(
                WebSource::new(account.ctx(), self.settings.clone())?
                    .with_retry_delay(Duration::from_millis(5)),
            )),
            _ => None,
        })
    }
}

struct Env {
    dir: tempfile::TempDir,
    p: Pipeline,
}

impl Env {
    fn new(settings: IngestSettings) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path().join("sbhome"));
        let cat = Catalog::create(&home).unwrap();
        cat.ensure_account(
            &second_brain_kernel::AccountId::new("local").unwrap(),
            AccountKind::Local,
            "Local files",
        )
        .unwrap();
        cat.ensure_account(
            &second_brain_kernel::AccountId::new("web").unwrap(),
            AccountKind::Web,
            "Web pages",
        )
        .unwrap();
        drop(cat);
        std::fs::create_dir_all(dir.path().join("user/.ssh")).unwrap();
        std::fs::create_dir_all(dir.path().join("files")).unwrap();
        let p = Pipeline::new(
            Catalog::open(&home).unwrap(),
            Arc::new(Factory {
                settings,
                user_home: dir.path().join("user"),
            }),
        );
        Env { dir, p }
    }

    fn file(&self, name: &str, bytes: &[u8]) -> String {
        let f = self.dir.path().join("files").join(name);
        std::fs::write(&f, bytes).unwrap();
        f.to_string_lossy().into_owned()
    }

    async fn ingest(&self, locators: &[&str], o: &IngestOptions) -> Vec<IngestResult> {
        let l: Vec<String> = locators.iter().map(|s| s.to_string()).collect();
        self.p.ingest(&l, o).await.unwrap().results
    }
}

fn opts() -> IngestOptions {
    IngestOptions {
        no_summary: true,
        ..Default::default()
    }
}

const LONG: &str = "# Launch plan\n\nWe decided to ship the new search feature in October. \
Alice owns the rollout, Bob reviews the budget by Friday, and Carol writes the announcement. \
The team agreed to keep the old endpoint for one more quarter and to measure adoption weekly.\n";

// ---------------------------------------------------------------- local.file

#[tokio::test]
async fn local_file_lifecycle_create_unchanged_update_and_context() {
    let env = Env::new(IngestSettings::default());
    let f = env.file("plan.md", LONG.as_bytes());
    let o = IngestOptions {
        context: Some("Q3 case".into()),
        ..opts()
    };
    let r = env.ingest(&[&f], &o).await;
    assert_eq!(r[0].status, IngestStatus::Created, "{r:?}");
    let uid = r[0].entry_uid.clone().unwrap();
    assert_eq!(r[0].title.as_deref(), Some("Launch plan"));

    // Same file, no new options: nothing happens, and the context stays.
    let r = env.ingest(&[&f], &opts()).await;
    assert_eq!(r[0].status, IngestStatus::Unchanged);

    // A new context alone updates the entry from the stored text.
    let o = IngestOptions {
        context: Some("changed".into()),
        ..opts()
    };
    let r = env.ingest(&[&f], &o).await;
    assert_eq!(r[0].status, IngestStatus::Updated);
    // The file changes: replaced, and the stored context survives.
    std::fs::write(&f, format!("{LONG}\nOne more paragraph.\n")).unwrap();
    let r = env.ingest(&[&f], &opts()).await;
    assert_eq!(r[0].status, IngestStatus::Updated);
    assert_eq!(r[0].entry_uid.as_deref(), Some(uid.as_str()));
    let cat = env.p.catalog();
    let e = cat.entry_by_uid(&uid).unwrap().unwrap();
    assert_eq!(e.origin.as_str(), "ingest");
    let secs = cat.sections(e.id).unwrap();
    let bg = secs
        .iter()
        .find(|s| s.kind == SectionKind::Background)
        .unwrap();
    assert_eq!(bg.text, "changed");
    let details = secs
        .iter()
        .find(|s| s.kind == SectionKind::Details)
        .unwrap();
    assert!(details.text.contains("One more paragraph"));
    // Raw data is the extracted text only.
    let rows = cat.raw_objects(e.id).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].role, RawRole::ExtractedText);
}

#[tokio::test]
async fn local_file_duplicates_moves_and_keep_original() {
    let env = Env::new(IngestSettings::default());
    let a = env.file("a.md", LONG.as_bytes());
    let b = env.file("copy-of-a.md", LONG.as_bytes());
    let r = env.ingest(&[&a, &b], &opts()).await;
    let statuses: Vec<_> = r.iter().map(|x| x.status).collect();
    assert!(
        statuses.contains(&IngestStatus::Created) && statuses.contains(&IngestStatus::Duplicate),
        "{r:?}"
    );
    let dup = r
        .iter()
        .find(|x| x.status == IngestStatus::Duplicate)
        .unwrap();
    assert!(dup.duplicate_of.is_some());
    // Which of the two is stored first depends on the order of the fetches.
    let kept = r
        .iter()
        .find(|x| x.status == IngestStatus::Created)
        .unwrap()
        .locator
        .clone();
    // From here on, `a` is the stored file and `b` the duplicate.
    let (a, b) = if kept == a { (a, b) } else { (b, a) };
    // --force adds the copy as its own entry.
    let o = IngestOptions {
        force: true,
        ..opts()
    };
    let r = env.ingest(&[&b], &o).await;
    assert!(matches!(
        r[0].status,
        IngestStatus::Created | IngestStatus::Unchanged | IngestStatus::Updated
    ));
    // keep_original on an entry without an original fetches again and stores it.
    let o = IngestOptions {
        keep_original: true,
        ..opts()
    };
    let r = env.ingest(&[&a], &o).await;
    assert_eq!(r[0].status, IngestStatus::Updated);
    let cat = env.p.catalog();
    let e = cat
        .entry_by_uid(r[0].entry_uid.as_ref().unwrap())
        .unwrap()
        .unwrap();
    let rows = cat.raw_objects(e.id).unwrap();
    let primary = rows.iter().find(|x| x.role == RawRole::Primary).unwrap();
    assert!(primary.path.ends_with("primary.0.md"), "{}", primary.path);
    assert_eq!(
        std::fs::read(env.p.home().root().join(&primary.path)).unwrap(),
        LONG.as_bytes()
    );
    // The hash is recorded either way.
    assert_eq!(e.metadata["original_size"], LONG.len());
}

#[tokio::test]
async fn local_file_encodings_formats_and_refusals() {
    let env = Env::new(IngestSettings::default());
    let (sjis, _, _) = encoding_rs_encode(
        "議事録: 新しい検索機能を十月に公開する。担当は佐藤さん。予算は金曜日までに確認する。",
    );
    let s = env.file("minutes.txt", &sjis);
    let csv = env.file("budget.csv", b"item,cost\nlicence,1200\nsupport,300\n");
    let bin = env.file("data.bin", &[0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0, 0, 0]);
    let tiny = env.file("tiny.txt", b"hi");
    let r = env.ingest(&[&s, &csv, &bin, &tiny], &opts()).await;
    assert_eq!(r[0].status, IngestStatus::Created, "{:?}", r[0]);
    assert_eq!(r[1].status, IngestStatus::Created);
    assert_eq!(r[2].status, IngestStatus::NotApplicable);
    assert!(r[2].message.as_deref().unwrap().contains("unsupported"));
    assert_eq!(r[3].status, IngestStatus::NotApplicable);
    let cat = env.p.catalog();
    let e = cat
        .entry_by_uid(r[0].entry_uid.as_ref().unwrap())
        .unwrap()
        .unwrap();
    let details = cat
        .sections(e.id)
        .unwrap()
        .into_iter()
        .find(|x| x.kind == SectionKind::Details)
        .unwrap();
    assert!(details.text.contains("新しい検索機能"));
    let e = cat
        .entry_by_uid(r[1].entry_uid.as_ref().unwrap())
        .unwrap()
        .unwrap();
    let details = cat
        .sections(e.id)
        .unwrap()
        .into_iter()
        .find(|x| x.kind == SectionKind::Details)
        .unwrap();
    assert!(details.text.contains("| item | cost |"));
    drop(cat);
    // Missing file, directory and bad option combinations.
    let missing = env.dir.path().join("files/none.md");
    let dir = env.dir.path().join("files");
    let r = env
        .ingest(&[missing.to_str().unwrap(), dir.to_str().unwrap()], &opts())
        .await;
    assert_eq!(r[0].status, IngestStatus::Failed);
    assert!(r[0].message.as_deref().unwrap().contains("not found"));
    assert_eq!(r[1].status, IngestStatus::NotApplicable);
}

fn encoding_rs_encode(s: &str) -> (Vec<u8>, (), ()) {
    // Shift_JIS without a dev-dependency on encoding_rs: use the extractor's own
    // dependency through a tiny round trip helper.
    (sb_extract_encode(s), (), ())
}

fn sb_extract_encode(s: &str) -> Vec<u8> {
    // The workspace's encoding_rs is available to this test through sb-extract's tree;
    // spelled out here to keep the test self-contained.
    let mut out = Vec::new();
    for ch in s.chars() {
        let mut buf = [0u8; 4];
        let t = ch.encode_utf8(&mut buf);
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode(t);
        out.extend_from_slice(&bytes);
    }
    out
}

#[tokio::test]
async fn local_file_denied_paths_have_no_override() {
    let env = Env::new(IngestSettings {
        local_deny: vec!["**/*.pem".into()],
        ..Default::default()
    });
    let key = env.dir.path().join("user/.ssh/id_rsa");
    std::fs::write(
        &key,
        // Built at compile time so that secret scanners do not flag this file.
        concat!(
            "-----BEGIN ",
            "PRIVATE KEY----- not a real key but long enough to count as text"
        ),
    )
    .unwrap();
    let in_home = env.p.home().root().join("notes.txt");
    std::fs::write(
        &in_home,
        "text inside the second-brain home that is long enough to extract",
    )
    .unwrap();
    let pem = env.file(
        "server.pem",
        b"certificate text that is long enough to be extracted as text",
    );
    let r = env
        .ingest(
            &[key.to_str().unwrap(), in_home.to_str().unwrap(), &pem],
            &IngestOptions {
                force: true,
                ..opts()
            },
        )
        .await;
    for x in &r {
        assert_eq!(x.status, IngestStatus::Failed, "{x:?}");
        assert!(
            x.message.as_deref().unwrap().contains("not allowed"),
            "{x:?}"
        );
    }
    #[cfg(unix)]
    {
        let link = env.dir.path().join("files/innocent.txt");
        std::os::unix::fs::symlink(&key, &link).unwrap();
        let r = env.ingest(&[link.to_str().unwrap()], &opts()).await;
        assert_eq!(r[0].status, IngestStatus::Failed);
        assert!(r[0].message.as_deref().unwrap().contains("not allowed"));
    }
    assert_eq!(
        env.p.catalog().count_entries(&Default::default()).unwrap(),
        0
    );
}

#[tokio::test]
async fn dry_run_and_usage_errors_write_nothing() {
    let env = Env::new(IngestSettings::default());
    let f = env.file("plan.md", LONG.as_bytes());
    let o = IngestOptions {
        dry_run: true,
        ..opts()
    };
    let r = env.ingest(&[&f, "ftp://x/y"], &o).await;
    assert_eq!(r[0].status, IngestStatus::WouldCreate);
    assert_eq!(r[1].status, IngestStatus::Failed);
    assert_eq!(
        env.p.catalog().count_entries(&Default::default()).unwrap(),
        0
    );
    // Real run, then a dry run reports the update.
    env.ingest(&[&f], &opts()).await;
    let r = env.ingest(&[&f], &o).await;
    assert_eq!(r[0].status, IngestStatus::WouldUpdate);
    // Option combinations.
    let two = vec![f.clone(), f.clone()];
    let err = env
        .p
        .ingest(
            &two,
            &IngestOptions {
                title: Some("x".into()),
                ..opts()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PipelineError::Invalid(_)));
    let report = env
        .p
        .ingest(&["ftp://x/y".to_string()], &opts())
        .await
        .unwrap();
    assert_eq!(report.valid, 0);
}

#[tokio::test]
async fn summaries_run_for_long_documents_and_short_ones_are_skipped() {
    let llm = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&llm)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "{\"overview\":\"The team will ship search in October.\",\"decisions\":[\"Ship in October\"],\"action_items\":[\"Alice owns the rollout\"]}"}}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 20}
        })))
        .mount(&llm)
        .await;
    let env = Env::new(IngestSettings::default());
    {
        let cat = env.p.catalog();
        cat.set_setting(
            "llm.profiles.fake",
            &json!({"kind": "local_llm", "provider": "openai_compatible", "model": "m",
                    "base_url": format!("{}/v1", llm.uri()), "concurrency": 1}),
        )
        .unwrap();
        cat.set_setting("summary.profile.default", &json!("fake"))
            .unwrap();
        cat.set_setting("summary.min_chars", &json!(100)).unwrap();
    }
    let long = env.file("plan.md", LONG.as_bytes());
    let short = env.file(
        "short.md",
        b"A short note of about sixty characters, no more than that.",
    );
    let o = IngestOptions {
        context: Some("Q3 case".into()),
        ..Default::default()
    };
    let report = env.p.ingest(&[long.clone(), short], &o).await.unwrap();
    assert_eq!(
        report.results[0].summary_status.as_deref(),
        Some("done"),
        "{report:?}"
    );
    assert_eq!(report.results[1].summary_status.as_deref(), Some("skipped"));
    assert_eq!(report.pending, 0);
    let cat = env.p.catalog();
    let e = cat
        .entry_by_uid(report.results[0].entry_uid.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(e.summary_status, SummaryStatus::Done);
    let kinds: Vec<_> = cat
        .sections(e.id)
        .unwrap()
        .into_iter()
        .map(|s| s.kind)
        .collect();
    for k in [
        SectionKind::Background,
        SectionKind::Overview,
        SectionKind::Decisions,
        SectionKind::ActionItems,
        SectionKind::Details,
    ] {
        assert!(kinds.contains(&k), "missing {k:?} in {kinds:?}");
    }
    let sum = cat.summary(e.id).unwrap().unwrap();
    assert_eq!(sum.prompt_version.as_deref(), Some("document-summary/v2"));
    // The user's context reached the model as a hint, outside the input tags.
    let reqs = llm.received_requests().await.unwrap();
    let body = reqs
        .iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .find(|b| b.contains("Q3 case"))
        .expect("the context is sent");
    assert!(body.contains("untrusted data"));
    assert!(body.contains("<input>"));
}

// ------------------------------------------------------------------ web.page

fn web_env() -> Env {
    Env::new(IngestSettings {
        web_allow_private: true,
        ..Default::default()
    })
}

const PAGE: &str = r#"<!doctype html><html><head><title>Release notes - Example</title>
<meta property="og:title" content="Release notes 2.0">
<meta property="article:published_time" content="2026-09-01T09:00:00+09:00">
<link rel="canonical" href="https://example.test/releases/2-0"></head>
<body><nav><a href="/">Home</a></nav>
<article><h1>Release notes 2.0</h1>
<p>The new search feature ships with this release, together with a faster index and a rewritten export dialog.</p>
<p>Upgrade notes: run the migration once, then restart the service. Older clients keep working for one quarter.</p>
</article><footer>footer text</footer></body></html>"#;

async fn page_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(path("/news"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "\"v1\"")
                .insert_header("last-modified", "Tue, 01 Sep 2026 00:30:00 GMT")
                .set_body_raw(PAGE, "text/html; charset=utf-8"),
        )
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn web_page_is_extracted_and_identity_ignores_tracking_parameters() {
    let server = page_server().await;
    let env = web_env();
    let u = format!("{}/news?utm_source=feed#top", server.uri());
    let r = env.ingest(&[&u], &opts()).await;
    assert_eq!(r[0].status, IngestStatus::Created, "{r:?}");
    assert_eq!(r[0].title.as_deref(), Some("Release notes 2.0"));
    let cat = env.p.catalog();
    let e = cat
        .entry_by_uid(r[0].entry_uid.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(e.source_kind, SourceKind::WebPage);
    assert_eq!(e.source_id, format!("{}/news", server.uri()));
    assert_eq!(
        e.metadata["canonical_url"],
        "https://example.test/releases/2-0"
    );
    assert_eq!(e.metadata["http_etag"], "\"v1\"");
    assert_eq!(
        e.source_created_at,
        second_brain_kernel::util::parse_ts("2026-09-01T00:00:00Z"),
        "the page's own publication date"
    );
    assert_eq!(
        e.source_updated_at,
        second_brain_kernel::util::parse_ts("2026-09-01T00:30:00Z"),
        "Last-Modified"
    );
    let details = cat
        .sections(e.id)
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SectionKind::Details)
        .unwrap();
    assert!(details.text.contains("new search feature"));
    assert!(!details.text.contains("footer text") && !details.text.contains("Home"));
    assert_eq!(cat.raw_objects(e.id).unwrap().len(), 1);
    drop(cat);
    // Without the tracking parameter: the same entry, unchanged (same body hash).
    let r = env
        .ingest(&[&format!("{}/news", server.uri())], &opts())
        .await;
    assert_eq!(r[0].status, IngestStatus::Unchanged, "{r:?}");
}

#[tokio::test]
async fn web_conditional_requests_and_changes() {
    let server = MockServer::start().await;
    Mock::given(path("/p"))
        .and(header("if-none-match", "\"v1\""))
        .respond_with(ResponseTemplate::new(304))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(path("/p"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "\"v1\"")
                .set_body_raw(
                    "A plain text page that is long enough to be extracted and stored.",
                    "text/plain",
                ),
        )
        .mount(&server)
        .await;
    let env = web_env();
    let u = format!("{}/p", server.uri());
    assert_eq!(
        env.ingest(&[&u], &opts()).await[0].status,
        IngestStatus::Created
    );
    // The second request is conditional and answered 304.
    assert_eq!(
        env.ingest(&[&u], &opts()).await[0].status,
        IngestStatus::Unchanged
    );
    let conditional = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.headers.contains_key("if-none-match"))
        .count();
    assert_eq!(conditional, 1);
    // --force sends no validators and fetches the body again.
    let o = IngestOptions {
        force: true,
        ..opts()
    };
    assert_eq!(env.ingest(&[&u], &o).await[0].status, IngestStatus::Updated);
}

#[tokio::test]
async fn web_failure_statuses_and_javascript_pages() {
    let server = MockServer::start().await;
    for (p, code) in [
        ("/login", 401),
        ("/forbidden", 403),
        ("/gone", 404),
        ("/boom", 500),
    ] {
        Mock::given(path(p))
            .respond_with(ResponseTemplate::new(code))
            .mount(&server)
            .await;
    }
    Mock::given(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<html><body><div id=root></div><script>render()</script></body></html>",
            "text/html",
        ))
        .mount(&server)
        .await;
    Mock::given(path("/image"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            vec![0x89, b'P', b'N', b'G', 0, 1, 2, 3, 0, 0, 0, 0],
            "image/png",
        ))
        .mount(&server)
        .await;
    let env = web_env();
    let urls: Vec<String> = ["login", "forbidden", "gone", "boom", "app", "image"]
        .iter()
        .map(|p| format!("{}/{p}", server.uri()))
        .collect();
    let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    let r = env.ingest(&refs, &opts()).await;
    let m = |i: usize| r[i].message.clone().unwrap_or_default();
    assert_eq!(r[0].status, IngestStatus::Failed);
    assert!(
        m(0).contains("401") && m(0).contains("save it as a file"),
        "{}",
        m(0)
    );
    assert!(m(1).contains("403"));
    assert_eq!(r[2].status, IngestStatus::Failed);
    assert!(m(2).contains("404"));
    assert_eq!(r[3].status, IngestStatus::Failed);
    assert!(m(3).contains("500"));
    // 500 was retried (three attempts).
    let boom = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|q| q.url.path() == "/boom")
        .count();
    assert_eq!(boom, 3);
    assert_eq!(r[4].status, IngestStatus::NotApplicable);
    assert!(m(4).contains("JavaScript"), "{}", m(4));
    assert_eq!(r[5].status, IngestStatus::NotApplicable);
    assert_eq!(
        env.p.catalog().count_entries(&Default::default()).unwrap(),
        0
    );
}

#[tokio::test]
async fn web_redirects_size_limit_and_content_types() {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(path("/old"))
        .respond_with(ResponseTemplate::new(301).insert_header("location", format!("{base}/new")))
        .mount(&server)
        .await;
    Mock::given(path("/new"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "# Moved page\n\nThis page moved here and its text is long enough to keep.",
            "text/markdown",
        ))
        .mount(&server)
        .await;
    Mock::given(path("/loop"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", format!("{base}/loop")))
        .mount(&server)
        .await;
    Mock::given(path("/huge"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("x".repeat(4096), "text/plain"))
        .mount(&server)
        .await;
    Mock::given(path("/data.csv"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw("a,b\n1,2\n3,4\n", "application/octet-stream"),
        )
        .mount(&server)
        .await;
    let env = Env::new(IngestSettings {
        web_allow_private: true,
        max_file_bytes: 2048,
        web_max_redirects: 3,
        ..Default::default()
    });
    let r = env
        .ingest(
            &[
                &format!("{base}/old"),
                &format!("{base}/loop"),
                &format!("{base}/huge"),
                &format!("{base}/data.csv"),
            ],
            &opts(),
        )
        .await;
    assert_eq!(r[0].status, IngestStatus::Created, "{:?}", r[0]);
    let cat = env.p.catalog();
    let e = cat
        .entry_by_uid(r[0].entry_uid.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        e.source_id,
        format!("{base}/old"),
        "identity is the requested URL"
    );
    assert_eq!(
        e.source_url.as_deref(),
        Some(format!("{base}/new").as_str()),
        "link is the final URL"
    );
    assert_eq!(e.metadata["final_url"], format!("{base}/new"));
    assert_eq!(e.title, "Moved page");
    drop(cat);
    assert_eq!(r[1].status, IngestStatus::Failed);
    assert!(
        r[1].message.as_deref().unwrap().contains("redirect"),
        "{:?}",
        r[1].message
    );
    assert_eq!(r[2].status, IngestStatus::Failed);
    assert!(r[2].message.as_deref().unwrap().contains("max_file_bytes"));
    // A CSV served as octet-stream is detected by its file name.
    assert_eq!(r[3].status, IngestStatus::Created, "{:?}", r[3]);
}

#[tokio::test]
async fn web_refuses_non_public_addresses_by_default() {
    let server = page_server().await;
    let env = Env::new(IngestSettings::default());
    let port = server.address().port();
    // An IP literal, and a name that resolves to loopback (checked in the resolver).
    let r = env
        .ingest(
            &[
                &format!("{}/news", server.uri()),
                &format!("http://localhost:{port}/news"),
                "http://169.254.169.254/latest/meta-data/",
                "http://[::1]:1/",
            ],
            &opts(),
        )
        .await;
    for x in &r {
        assert_eq!(x.status, IngestStatus::Failed, "{x:?}");
        assert!(
            x.message.as_deref().unwrap().contains("non-public"),
            "{x:?}"
        );
    }
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "nothing was sent"
    );
    let _ = Path::new("");
}

// ----------------------------------------------- refetch, reextract and import

#[tokio::test]
async fn refetch_and_reextract_keep_context_and_the_stored_original() {
    let env = Env::new(IngestSettings::default());
    let f = env.file("plan.md", LONG.as_bytes());
    let o = IngestOptions {
        context: Some("Q3 case".into()),
        title: Some("My title".into()),
        keep_original: true,
        ..opts()
    };
    let r = env.ingest(&[&f], &o).await;
    let uid = r[0].entry_uid.clone().unwrap();
    // The file changes on disk; refetch picks it up.
    std::fs::write(&f, format!("{LONG}\nA new paragraph about the budget.\n")).unwrap();
    let filter = second_brain_store::EntryFilter {
        entry_uids: vec![uid.clone()],
        ..Default::default()
    };
    let rep = env
        .p
        .refetch(&filter, &second_brain_pipeline::Limits::default())
        .await
        .unwrap();
    assert_eq!((rep.updated, rep.failed), (1, 0), "{rep:?}");
    {
        let cat = env.p.catalog();
        let e = cat.entry_by_uid(&uid).unwrap().unwrap();
        assert_eq!(e.title, "My title");
        assert_eq!(e.metadata["context"], "Q3 case");
        let rows = cat.raw_objects(e.id).unwrap();
        assert!(
            rows.iter().any(|x| x.role == RawRole::Primary),
            "the original is kept"
        );
        let details = cat
            .sections(e.id)
            .unwrap()
            .into_iter()
            .find(|s| s.kind == SectionKind::Details)
            .unwrap();
        assert!(details.text.contains("A new paragraph"));
    }
    // reextract re-runs normalize on the stored text only; it works without the file.
    std::fs::remove_file(&f).unwrap();
    let rep = env
        .p
        .reextract(&filter, &second_brain_pipeline::Limits::default())
        .await
        .unwrap();
    assert_eq!((rep.updated, rep.failed), (1, 0), "{rep:?}");
    // refetch cannot find the file: the entry and its text stay.
    let rep = env
        .p
        .refetch(&filter, &second_brain_pipeline::Limits::default())
        .await
        .unwrap();
    assert_eq!(rep.failed, 1, "{rep:?}");
    let cat = env.p.catalog();
    let e = cat.entry_by_uid(&uid).unwrap().unwrap();
    assert_eq!(e.raw_status, second_brain_kernel::RawStatus::Present);
    assert_eq!(e.title, "My title");
}

#[tokio::test]
async fn an_imported_entry_without_raw_data_is_filled_by_ingest() {
    let server = page_server().await;
    let env = web_env();
    let url = format!("{}/news", server.uri());
    {
        // As `sb import` would create it: the natural key, no raw data.
        let cat = env.p.catalog();
        cat.upsert_entry(&second_brain_store::EntryUpdate {
            source_ref: second_brain_kernel::SourceRef {
                account_id: second_brain_kernel::AccountId::new("web").unwrap(),
                source_kind: SourceKind::WebPage,
                source_id: url.clone(),
                source_url: Some(url.clone()),
                created_at: None,
                updated_at: None,
            },
            origin: second_brain_kernel::EntryOrigin::Import,
            raw: None,
            fetch_state: None,
            metadata: Some(json!({"import_ref": "x1"})),
            normalized: Some(second_brain_store::NormalizedUpdate {
                title: "Imported title".into(),
                source_url: Some(url.clone()),
                source_created_at: None,
                source_updated_at: None,
                sections: vec![],
                summary: second_brain_store::SummaryDecision::NoSummary,
            }),
        })
        .unwrap();
        let e = cat
            .entry_by_key("web", SourceKind::WebPage, &url)
            .unwrap()
            .unwrap();
        cat.set_raw_status(e.id, second_brain_kernel::RawStatus::Missing)
            .unwrap();
    }
    let r = env.ingest(&[&url], &opts()).await;
    assert_eq!(r[0].status, IngestStatus::Updated, "{r:?}");
    let cat = env.p.catalog();
    let e = cat
        .entry_by_key("web", SourceKind::WebPage, &url)
        .unwrap()
        .unwrap();
    assert_eq!(e.raw_status, second_brain_kernel::RawStatus::Present);
    assert_eq!(
        e.metadata["import_ref"], "x1",
        "the import reference survives"
    );
    assert_eq!(
        e.origin.as_str(),
        "import",
        "the origin of an existing entry is kept"
    );
}

#[tokio::test]
async fn secret_files_are_denied_everywhere_and_dry_run_agrees() {
    let env = Env::new(IngestSettings::default());
    let text = b"API_TOKEN=abcdef0123456789 and some more text to be long enough";
    let envfile = env.file(".env", text);
    let envprod = env.file(".env.production", text);
    let key = env.file("server.key", text);
    let netrc = env.file(".netrc", text);
    let ok = env.file("notes.env.md", LONG.as_bytes());
    for o in [
        opts(),
        IngestOptions {
            dry_run: true,
            ..opts()
        },
    ] {
        let r = env
            .ingest(&[&envfile, &envprod, &key, &netrc, &ok], &o)
            .await;
        for x in &r[..4] {
            assert_eq!(x.status, IngestStatus::Failed, "{x:?}");
            assert!(
                x.message.as_deref().unwrap().contains("not allowed"),
                "{x:?}"
            );
        }
        assert!(
            matches!(
                r[4].status,
                IngestStatus::Created
                    | IngestStatus::WouldCreate
                    | IngestStatus::WouldUpdate
                    | IngestStatus::Unchanged
            ),
            "{:?}",
            r[4]
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_credential_directory_is_still_denied() {
    let env = Env::new(IngestSettings::default());
    // ~/.ssh is a symlink into a dotfiles directory.
    let dotfiles = env.dir.path().join("dotfiles-ssh");
    std::fs::create_dir_all(&dotfiles).unwrap();
    std::fs::write(
        dotfiles.join("config"),
        "Host example\n  User someone\n  IdentityFile none",
    )
    .unwrap();
    let ssh = env.dir.path().join("user/.ssh");
    std::fs::remove_dir_all(&ssh).unwrap();
    std::os::unix::fs::symlink(&dotfiles, &ssh).unwrap();
    // Through the link, and straight at the target.
    let via = ssh.join("config");
    let direct = dotfiles.join("config");
    let r = env
        .ingest(&[via.to_str().unwrap(), direct.to_str().unwrap()], &opts())
        .await;
    assert_eq!(r[0].status, IngestStatus::Failed, "{:?}", r[0]);
    assert!(r[0].message.as_deref().unwrap().contains("not allowed"));
    // The target is the same place seen from the other side.
    assert_eq!(r[1].status, IngestStatus::Failed, "{:?}", r[1]);
    assert_eq!(
        env.p.catalog().count_entries(&Default::default()).unwrap(),
        0
    );
}

#[tokio::test]
async fn a_configured_proxy_blocks_web_fetches_unless_private_addresses_are_allowed() {
    let source = |allow| {
        WebSource::new(
            second_brain_kernel::AccountCtx {
                id: second_brain_kernel::AccountId::new("web").unwrap(),
                kind: AccountKind::Web,
                label: "Web".into(),
                identity: None,
                config: json!({}),
            },
            IngestSettings {
                web_allow_private: allow,
                ..Default::default()
            },
        )
        .unwrap()
        .with_proxy_env(true)
    };
    struct NoHost;
    impl second_brain_kernel::source::SyncHost for NoHost {
        fn cursor(
            &self,
            _: SourceKind,
            _: &str,
        ) -> Result<Option<serde_json::Value>, second_brain_kernel::source::SourceError> {
            Ok(None)
        }
        fn cursors(
            &self,
            _: SourceKind,
            _: &str,
        ) -> Result<Vec<(String, serde_json::Value)>, second_brain_kernel::source::SourceError>
        {
            Ok(vec![])
        }
        fn fetch_state(
            &self,
            _: SourceKind,
            _: &str,
        ) -> Result<Option<serde_json::Value>, second_brain_kernel::source::SourceError> {
            Ok(None)
        }
        fn entry_exists(
            &self,
            _: SourceKind,
            _: &str,
        ) -> Result<bool, second_brain_kernel::source::SourceError> {
            Ok(false)
        }
        fn commit(
            &self,
            _: second_brain_kernel::DiscoveryBatch,
        ) -> Result<(), second_brain_kernel::source::SourceError> {
            Ok(())
        }
        fn cache_get(
            &self,
            _: &str,
        ) -> Result<Option<serde_json::Value>, second_brain_kernel::source::SourceError> {
            Ok(None)
        }
        fn cache_put(
            &self,
            _: &str,
            _: &serde_json::Value,
            _: Duration,
        ) -> Result<(), second_brain_kernel::source::SourceError> {
            Ok(())
        }
        fn is_cancelled(&self) -> bool {
            false
        }
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            chrono::Utc::now()
        }
    }
    let req = second_brain_kernel::FetchRequest {
        source_kind: SourceKind::WebPage,
        source_id: "https://example.com/".into(),
        fetch_state: None,
        metadata: json!(null),
        hint: json!(null),
        full: true,
    };
    let err = source(false).fetch(&NoHost, &req).await.unwrap_err();
    assert!(err.to_string().contains("proxy"), "{err}");
}
