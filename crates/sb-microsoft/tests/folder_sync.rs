//! Folder sync for SharePoint and OneDrive through the pipeline against mock Entra
//! and Graph endpoints. All names, IDs and texts are synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use second_brain_kernel::clock::FixedClock;
use second_brain_kernel::document::IngestSettings;
use second_brain_kernel::source::Source;
use second_brain_kernel::{AccountId, AccountKind, EntryOrigin, RunStatus, Secret, SourceKind};
use second_brain_microsoft::{
    GraphApi, MicrosoftClient, MicrosoftConfig, MicrosoftSource, RefreshSink, TokenProvider,
};
use second_brain_pipeline::sync::SyncOptions;
use second_brain_pipeline::{Pipeline, PipelineError, SourceFactory};
use second_brain_store::{Account, Catalog, Home};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const CID: &str = "11111111-2222-3333-4444-555555555555";
const DRIVE: &str = "b!DRIVE";
const TEXT: &str = "# Launch plan\n\nThe decision is to ship the beta in March. Owner: Alice.\n";

struct Factory(String);

impl SourceFactory for Factory {
    fn source(
        &self,
        account: &Account,
        _c: &Catalog,
    ) -> Result<Option<Arc<dyn Source>>, PipelineError> {
        let client = MicrosoftClient::new(CID, "common")
            .unwrap()
            .with_endpoints(format!("{}/auth", self.0), format!("{}/token", self.0));
        let sink: RefreshSink = Arc::new(|_| Ok(()));
        let tokens = Arc::new(TokenProvider::new(
            client,
            Secret::new(format!("rt-{}", account.id)),
            sink,
        ));
        let api = GraphApi::new(tokens)?
            .with_base(&self.0)
            .allow_http_downloads()?;
        Ok(Some(Arc::new(MicrosoftSource::new(
            account.ctx(),
            Some(api),
            IngestSettings::default(),
        ))))
    }
}

struct Tenant {
    f1_etag: String,
    /// The `@odata.nextLink` of the first page; `None` = the mock server itself.
    next_link: Option<String>,
}

type State = Arc<Mutex<Tenant>>;

fn item(id: &str, name: &str, etag: &str, extra: Value) -> Value {
    let mut v = json!({
        "id": id, "name": name, "size": 60, "eTag": etag,
        "webUrl": format!("https://contoso.sharepoint.com/sites/x/Minutes/{name}"),
        "createdDateTime": "2026-09-01T00:00:00Z",
        "lastModifiedDateTime": "2026-09-05T00:00:00Z",
        "file": {"mimeType": "text/markdown"},
        "parentReference": {"driveId": DRIVE, "driveType": "documentLibrary"},
    });
    for (k, x) in extra.as_object().cloned().unwrap_or_default() {
        v[k] = x;
    }
    v
}

fn folder_item(id: &str, name: &str) -> Value {
    json!({"id": id, "name": name, "folder": {"childCount": 1},
           "parentReference": {"driveId": DRIVE}})
}

async fn mount_file(s: &MockServer, it: &Value, body: &str) {
    let id = it["id"].as_str().unwrap().to_string();
    Mock::given(path(format!("/drives/{DRIVE}/items/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(it))
        .mount(s)
        .await;
    Mock::given(path(format!("/drives/{DRIVE}/items/{id}/content")))
        .respond_with(
            ResponseTemplate::new(302).insert_header("Location", format!("{}/dl/{id}", s.uri())),
        )
        .mount(s)
        .await;
    Mock::given(path(format!("/dl/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(s)
        .await;
}

async fn mock(state: State) -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("refresh_token=rt-work"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at-work", "expires_in": 3600
        })))
        .mount(&s)
        .await;
    // The root folder.
    Mock::given(path(format!("/drives/{DRIVE}/items/ROOT")))
        .respond_with(ResponseTemplate::new(200).set_body_json(folder_item("ROOT", "Minutes")))
        .mount(&s)
        .await;
    // Children of ROOT in two pages. The second page is matched first.
    let uri = s.uri();
    Mock::given(path(format!("/drives/{DRIVE}/items/ROOT/children")))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [
            folder_item("SUB", "2026"),
            // A folder shared into OneDrive is not followed.
            json!({"id": "REM", "remoteItem": folder_item("REMT", "Shared folder")}),
            item("F4", "photo.png", "e4", json!({})),
        ]})))
        .with_priority(1)
        .mount(&s)
        .await;
    let st = state.clone();
    Mock::given(path(format!("/drives/{DRIVE}/items/ROOT/children")))
        .respond_with(move |_: &Request| {
            let t = st.lock().unwrap();
            let next = t
                .next_link
                .clone()
                .unwrap_or_else(|| format!("{uri}/drives/{DRIVE}/items/ROOT/children?page=2"));
            ResponseTemplate::new(200).set_body_json(json!({
                "value": [
                    item("F1", "plan.md", &t.f1_etag, json!({})),
                    item("F2", "notes.one", "e2", json!({})),
                    item("F3", "OneNote", "e3", json!({"package": {"type": "oneNote"}, "file": null})),
                ],
                "@odata.nextLink": next,
            }))
        })
        .mount(&s)
        .await;
    Mock::given(path(format!("/drives/{DRIVE}/items/SUB/children")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [
            item("F5", "q3.md", "e5", json!({})),
        ]})))
        .mount(&s)
        .await;
    let st = state.clone();
    Mock::given(path(format!("/drives/{DRIVE}/items/F1")))
        .respond_with(move |_: &Request| {
            let etag = st.lock().unwrap().f1_etag.clone();
            ResponseTemplate::new(200).set_body_json(item("F1", "plan.md", &etag, json!({})))
        })
        .mount(&s)
        .await;
    Mock::given(path(format!("/drives/{DRIVE}/items/F1/content")))
        .respond_with(
            ResponseTemplate::new(302).insert_header("Location", format!("{}/dl/F1", s.uri())),
        )
        .mount(&s)
        .await;
    Mock::given(path("/dl/F1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(TEXT))
        .mount(&s)
        .await;
    mount_file(&s, &item("F5", "q3.md", "e5", json!({})), TEXT).await;
    s
}

struct Env {
    _dir: tempfile::TempDir,
    p: Pipeline,
    server: MockServer,
    state: State,
}

async fn env(with_folder: bool) -> Env {
    let state: State = Arc::new(Mutex::new(Tenant {
        f1_etag: "e1".into(),
        next_link: None,
    }));
    let server = mock(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let cat = Catalog::create(&home).unwrap();
    let mut config = MicrosoftConfig::new(CID, "common").to_value();
    if with_folder {
        config["folders"] = json!([{
            "key": format!("{DRIVE}/ROOT"), "label": "Minutes",
            "url": "https://contoso.sharepoint.com/sites/x/Minutes"
        }]);
    }
    cat.add_account(
        &AccountId::new("work").unwrap(),
        AccountKind::Microsoft,
        "Work",
        Some("tid:oid"),
        &config,
    )
    .unwrap();
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
        state,
    }
}

fn opts() -> SyncOptions {
    SyncOptions {
        no_summary: true,
        ..Default::default()
    }
}

fn entry(e: &Env, id: &str) -> Option<second_brain_store::Entry> {
    e.p.catalog()
        .entry_by_key("work", SourceKind::MicrosoftFile, &format!("{DRIVE}/{id}"))
        .unwrap()
}

async fn requests_to(s: &MockServer, p: &str) -> usize {
    s.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == p)
        .count()
}

#[tokio::test]
async fn files_across_pages_and_subfolders_become_entries() {
    let e = env(true).await;
    let r = e.p.sync(&opts()).await.unwrap();
    assert!(r.stats.errors.is_empty(), "{:?}", r.stats.errors);
    assert_eq!(r.status, Some(RunStatus::Ok));
    for (id, folder_path) in [("F1", "Minutes"), ("F5", "Minutes / 2026")] {
        let en = entry(&e, id).unwrap_or_else(|| panic!("{id} is missing"));
        assert_eq!(en.origin, EntryOrigin::Sync);
        assert_eq!(en.metadata["folder_path"], folder_path);
        assert_eq!(en.metadata["sync_folder"], format!("{DRIVE}/ROOT"));
    }
    // OneNote, images, a package and a shared folder are not entered or followed.
    for id in ["F2", "F3", "F4", "REM", "REMT"] {
        assert!(entry(&e, id).is_none(), "{id}");
    }
    assert_eq!(
        requests_to(&e.server, &format!("/drives/{DRIVE}/items/REMT/children")).await,
        0
    );
}

#[tokio::test]
async fn only_changed_files_are_fetched_again() {
    let e = env(true).await;
    e.p.sync(&opts()).await.unwrap();
    let content = format!("/drives/{DRIVE}/items/F1/content");
    assert_eq!(requests_to(&e.server, &content).await, 1);
    let r = e.p.sync(&opts()).await.unwrap();
    assert!(r.stats.errors.is_empty(), "{:?}", r.stats.errors);
    assert_eq!(requests_to(&e.server, &content).await, 1, "unchanged");
    e.state.lock().unwrap().f1_etag = "e1-new".into();
    let r = e.p.sync(&opts()).await.unwrap();
    assert_eq!(
        r.stats.sources["work/microsoft.file"].updated, 1,
        "{:?}",
        r.stats
    );
    assert_eq!(requests_to(&e.server, &content).await, 2);
    assert_eq!(
        entry(&e, "F1").unwrap().fetch_state.unwrap()["etag"],
        "e1-new"
    );
}

#[tokio::test]
async fn a_next_page_link_to_another_host_is_not_followed() {
    let e = env(true).await;
    e.state.lock().unwrap().next_link = Some("https://evil.example.test/steal".into());
    let r = e.p.sync(&opts()).await.unwrap();
    assert_eq!(r.status, Some(RunStatus::Partial));
    assert!(
        r.stats.errors.iter().any(|m| m.contains("another host")),
        "{:?}",
        r.stats.errors
    );
    // Nothing was queued from the half-read folder.
    assert!(entry(&e, "F1").is_none());
    let open = e.p.catalog().open_issues().unwrap();
    assert!(open.iter().any(|i| i.code == "folder.failed"), "{open:?}");
}

#[tokio::test]
async fn an_account_without_folders_is_not_synced() {
    let e = env(false).await;
    let r = e.p.sync(&opts()).await.unwrap();
    assert!(r.stats.errors.is_empty(), "{:?}", r.stats.errors);
    assert_eq!(
        requests_to(&e.server, &format!("/drives/{DRIVE}/items/ROOT/children")).await,
        0
    );
}
