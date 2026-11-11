//! The link stage (ADR-0022) against mock Google APIs. Slack entries are made
//! directly in the catalog; all names, IDs and texts are synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use second_brain_google::{GoogleApi, GoogleSource, OAuthClient, TokenProvider};
use second_brain_kernel::clock::FixedClock;
use second_brain_kernel::document::IngestSettings;
use second_brain_kernel::links::{DiscoveredLink, classify_link};
use second_brain_kernel::source::Source;
use second_brain_kernel::{
    AccountId, AccountKind, EntryOrigin, LinkStatus, Secret, SourceKind, SourceRef,
};
use second_brain_pipeline::ingest::IngestOptions;
use second_brain_pipeline::link_stage::LinkStageOptions;
use second_brain_pipeline::{Pipeline, PipelineError, SourceFactory};
use second_brain_store::{Account, Catalog, EntryUpdate, Home, NormalizedUpdate, SummaryDecision};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DOC_MD: &str = "# Design memo\n\nThe decision is to use CSV for the export format.\n";

fn doc_url(id: &str) -> String {
    format!("https://docs.google.com/document/d/{id}/edit")
}

struct Factory(String);

impl SourceFactory for Factory {
    fn source(
        &self,
        account: &Account,
        _c: &Catalog,
    ) -> Result<Option<Arc<dyn Source>>, PipelineError> {
        let client =
            OAuthClient::from_json(r#"{"installed":{"client_id":"c","client_secret":"s"}}"#)
                .unwrap()
                .with_endpoints(format!("{}/auth", self.0), format!("{}/token", self.0));
        let tokens = Arc::new(TokenProvider::new(client, Secret::new("rt")));
        let api = GoogleApi::new(tokens)?.with_base(&self.0);
        Ok(Some(Arc::new(GoogleSource::new(
            account.ctx(),
            Some(api),
            IngestSettings::default(),
        )?)))
    }
}

fn meta(id: &str, name: &str) -> Value {
    json!({"id": id, "name": name, "mimeType": "application/vnd.google-apps.document",
           "createdTime": "2026-09-01T00:00:00Z", "modifiedTime": "2026-09-02T00:00:00Z",
           "webViewLink": doc_url(id)})
}

async fn mock() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "at", "expires_in": 3600})),
        )
        .mount(&server)
        .await;
    for (id, name) in [
        ("DOC1", "Design memo"),
        ("DOC2", "Second memo"),
        ("DOC3", "Third"),
    ] {
        Mock::given(path(format!("/drive/v3/files/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(meta(id, name)))
            .mount(&server)
            .await;
        Mock::given(path(format!("/drive/v3/files/{id}/export")))
            .and(query_param("mimeType", "text/markdown"))
            .respond_with(ResponseTemplate::new(200).set_body_string(DOC_MD))
            .mount(&server)
            .await;
    }
    Mock::given(path("/drive/v3/files/DENIED1"))
        .respond_with(ResponseTemplate::new(403).set_body_json(
            json!({"error": {"message": "no permission", "errors": [{"reason": "forbidden"}]}}),
        ))
        .mount(&server)
        .await;
    server
}

struct Env {
    _dir: tempfile::TempDir,
    p: Pipeline,
    server: MockServer,
}

/// A Slack account `acme` (with `slack_config`), and Google accounts as asked.
async fn env(slack_config: Value, google: &[&str]) -> Env {
    let server = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let cat = Catalog::create(&home).unwrap();
    cat.add_account(
        &AccountId::new("acme").unwrap(),
        AccountKind::Slack,
        "Acme",
        Some("T1:U1"),
        &slack_config,
    )
    .unwrap();
    for a in google {
        cat.add_account(
            &AccountId::new(*a).unwrap(),
            AccountKind::Google,
            a,
            Some(&format!("{a}@example.test")),
            &second_brain_google::default_config_json(&["docs".into()]),
        )
        .unwrap();
    }
    drop(cat);
    let clock = Arc::new(FixedClock::new(
        second_brain_kernel::util::parse_ts("2026-09-10T00:00:00Z").unwrap(),
    ));
    let p = Pipeline::new(
        Catalog::open_with_clock(&home, clock).unwrap(),
        Arc::new(Factory(server.uri())),
    );
    Env {
        _dir: dir,
        p,
        server,
    }
}

/// A Slack entry holding links to the given URLs. Returns its entry ID.
fn slack_entry(p: &Pipeline, id: &str, urls: &[String]) -> i64 {
    let links: Vec<DiscoveredLink> = urls
        .iter()
        .map(|u| DiscoveredLink {
            link: classify_link(u).unwrap(),
            message_ts: "1.0".into(),
            excerpt: format!("#dev, 2026-09-01\nBob: please read {}", "[linked document]"),
        })
        .collect();
    p.catalog()
        .upsert_entry(&EntryUpdate {
            source_ref: SourceRef {
                account_id: AccountId::new("acme").unwrap(),
                source_kind: SourceKind::SlackThread,
                source_id: id.into(),
                source_url: Some(format!("https://acme.slack.test/archives/{id}")),
                created_at: None,
                updated_at: None,
            },
            origin: EntryOrigin::Sync,
            raw: None,
            fetch_state: None,
            metadata: None,
            normalized: Some(NormalizedUpdate {
                title: "#dev design review".into(),
                source_url: None,
                source_created_at: None,
                source_updated_at: None,
                sections: vec![],
                summary: SummaryDecision::NoSummary,
                links,
            }),
        })
        .unwrap()
        .entry_id
}

async fn requests_to(s: &MockServer, p: &str) -> usize {
    s.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == p)
        .count()
}

fn link(p: &Pipeline, url: &str) -> second_brain_store::LinkRow {
    let key = classify_link(url).unwrap().key;
    p.catalog().link("acme", &key).unwrap().unwrap()
}

fn on() -> Value {
    json!({"ingest_links": ["google.doc"]})
}

#[tokio::test]
async fn enabled_links_are_ingested_with_link_origin_and_context() {
    let e = env(on(), &["work"]).await;
    let slack_id = slack_entry(
        &e.p,
        "C1:1.0",
        &[
            doc_url("DOC1"),
            doc_url("DENIED1"),
            "https://contoso.sharepoint.com/:w:/s/team/Eabc".into(),
        ],
    );
    let r =
        e.p.run_link_stage(&LinkStageOptions::default())
            .await
            .unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(
        (r.stats.due, r.stats.ingested, r.stats.unavailable),
        (2, 1, 1)
    );

    // The document is an entry of the Google account, made by the link.
    let l = link(&e.p, &doc_url("DOC1"));
    assert_eq!(l.status, LinkStatus::Ingested);
    let entry = e.p.catalog().entry(l.entry_id.unwrap()).unwrap().unwrap();
    assert_eq!(
        (entry.account_id.as_str(), entry.source_kind, entry.origin),
        ("work", SourceKind::GoogleDoc, EntryOrigin::Link)
    );
    assert!(
        entry.metadata["link_context"]
            .as_str()
            .unwrap()
            .contains("please read [linked document]")
    );
    let via = &entry.metadata["discovered_via"];
    assert_eq!(via["title"], "#dev design review");
    assert!(via["entry_uid"].as_str().is_some());
    let slack = e.p.catalog().entry(slack_id).unwrap().unwrap();
    assert_eq!(via["entry_uid"], json!(slack.entry_uid));

    // The unreadable link backs off; the SharePoint link (kind off) is untouched.
    let d = link(&e.p, &doc_url("DENIED1"));
    assert_eq!((d.status, d.attempts), (LinkStatus::Unavailable, 1));
    assert!(d.next_attempt_at.is_some());
    assert!(d.last_error.unwrap().contains("work"));
    let sp = link(&e.p, "https://contoso.sharepoint.com/:w:/s/team/Eabc");
    assert_eq!((sp.status, sp.attempts), (LinkStatus::Pending, 0));

    // A second run does not ask Drive about either again.
    let before = requests_to(&e.server, "/drive/v3/files/DENIED1").await;
    let r =
        e.p.run_link_stage(&LinkStageOptions::default())
            .await
            .unwrap();
    assert_eq!(r.stats.due, 0);
    assert_eq!(
        requests_to(&e.server, "/drive/v3/files/DENIED1").await,
        before
    );
    assert_eq!(requests_to(&e.server, "/drive/v3/files/DOC1").await, 1);
}

#[tokio::test]
async fn nothing_is_ingested_unless_a_kind_is_enabled() {
    let e = env(json!({}), &["work"]).await;
    slack_entry(&e.p, "C1:1.0", &[doc_url("DOC1")]);
    let r =
        e.p.run_link_stage(&LinkStageOptions::default())
            .await
            .unwrap();
    assert!(r.stats.is_empty(), "{:?}", r.stats);
    assert_eq!(requests_to(&e.server, "/drive/v3/files/DOC1").await, 0);
    assert_eq!(link(&e.p, &doc_url("DOC1")).status, LinkStatus::Pending);

    // The global setting enables it; an account value of [] turns it off again.
    e.p.catalog()
        .set_setting("links.ingest_kinds", &json!(["google.doc"]))
        .unwrap();
    let r =
        e.p.run_link_stage(&LinkStageOptions::default())
            .await
            .unwrap();
    assert_eq!(r.stats.ingested, 1);
}

#[tokio::test]
async fn an_account_can_turn_links_off_and_a_host_list_narrows_them() {
    let e = env(json!({"ingest_links": []}), &["work"]).await;
    e.p.catalog()
        .set_setting("links.ingest_kinds", &json!(["google.doc"]))
        .unwrap();
    slack_entry(&e.p, "C1:1.0", &[doc_url("DOC1")]);
    let r =
        e.p.run_link_stage(&LinkStageOptions::default())
            .await
            .unwrap();
    assert!(r.stats.is_empty(), "{:?}", r.stats);

    let e = env(
        json!({"ingest_links": ["google.doc"], "ingest_link_hosts": ["drive.google.com"]}),
        &["work"],
    )
    .await;
    slack_entry(&e.p, "C1:1.0", &[doc_url("DOC1")]);
    let r =
        e.p.run_link_stage(&LinkStageOptions::default())
            .await
            .unwrap();
    assert!(
        r.stats.is_empty(),
        "docs.google.com is not on the host list"
    );
}

#[tokio::test]
async fn without_an_account_the_link_waits_and_is_not_counted() {
    let e = env(on(), &[]).await;
    slack_entry(&e.p, "C1:1.0", &[doc_url("DOC1")]);
    let r =
        e.p.run_link_stage(&LinkStageOptions::default())
            .await
            .unwrap();
    assert_eq!(
        (r.stats.due, r.stats.waiting, r.stats.unavailable),
        (1, 1, 0)
    );
    let l = link(&e.p, &doc_url("DOC1"));
    assert_eq!((l.status, l.attempts), (LinkStatus::Pending, 0));
    assert!(l.last_error.unwrap().contains("no usable Google account"));
}

#[tokio::test]
async fn an_entry_that_exists_keeps_its_origin() {
    let e = env(on(), &["work"]).await;
    let o = IngestOptions {
        no_summary: true,
        ..Default::default()
    };
    e.p.ingest(&[doc_url("DOC1")], &o).await.unwrap();
    slack_entry(&e.p, "C1:1.0", &[doc_url("DOC1")]);
    let r =
        e.p.run_link_stage(&LinkStageOptions::default())
            .await
            .unwrap();
    assert_eq!(r.stats.ingested, 1, "{:?}", r.stats);
    let l = link(&e.p, &doc_url("DOC1"));
    let entry = e.p.catalog().entry(l.entry_id.unwrap()).unwrap().unwrap();
    assert_eq!(entry.origin, EntryOrigin::Ingest);
    assert!(entry.metadata.get("link_context").is_none());
}

#[tokio::test]
async fn at_most_max_per_run_links_are_fetched_and_the_rest_wait() {
    let e = env(on(), &["work"]).await;
    slack_entry(&e.p, "C1:1.0", &[doc_url("DOC1")]);
    slack_entry(&e.p, "C1:2.0", &[doc_url("DOC2")]);
    slack_entry(&e.p, "C1:3.0", &[doc_url("DOC3")]);
    let o = LinkStageOptions {
        max: Some(2),
        ..Default::default()
    };
    let r = e.p.run_link_stage(&o).await.unwrap();
    assert_eq!(
        (r.stats.due, r.stats.ingested, r.stats.deferred_by_limit),
        (3, 2, 1)
    );
    let pending = e.p.catalog().list_links(None, &[], 10).unwrap();
    assert_eq!(
        pending
            .iter()
            .filter(|l| l.status == LinkStatus::Pending)
            .count(),
        1
    );
    // The next run takes the rest.
    let r =
        e.p.run_link_stage(&LinkStageOptions::default())
            .await
            .unwrap();
    assert_eq!(r.stats.ingested, 1);
}

#[tokio::test]
async fn sync_runs_the_stage_alone_with_links_only() {
    let e = env(on(), &["work"]).await;
    slack_entry(&e.p, "C1:1.0", &[doc_url("DOC1"), doc_url("DENIED1")]);
    let opts = second_brain_pipeline::sync::SyncOptions {
        links_only: true,
        no_summary: true,
        ..Default::default()
    };
    let r = e.p.sync(&opts).await.unwrap();
    assert_eq!((r.stats.links.ingested, r.stats.links.unavailable), (1, 1));
    // The entry made by the stage is counted like any committed entry.
    assert_eq!(r.stats.sources["work/google.doc"].new, 1);
    assert_eq!(r.status, Some(second_brain_kernel::RunStatus::Ok));
    // The stats are stored with the run.
    let run = e.p.catalog().runs(Some("sync"), None, 1).unwrap().remove(0);
    assert_eq!(Some(run.id), r.run_id);
    assert_eq!(run.stats["links"]["ingested"], 1);
}
