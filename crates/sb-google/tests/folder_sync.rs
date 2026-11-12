//! Folder sync for Drive through the pipeline against mock Google APIs.
//! All names, IDs and texts are synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use second_brain_google::{GoogleApi, GoogleSource, OAuthClient, TokenProvider};
use second_brain_kernel::clock::FixedClock;
use second_brain_kernel::document::IngestSettings;
use second_brain_kernel::source::Source;
use second_brain_kernel::{
    AccountId, AccountKind, EntryOrigin, RunStatus, Secret, SourceKind, SourceRef,
};
use second_brain_pipeline::sync::SyncOptions;
use second_brain_pipeline::{Pipeline, PipelineError, SourceFactory};
use second_brain_store::{Account, Catalog, EntryUpdate, Home};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const FOLDER: &str = "application/vnd.google-apps.folder";
const DOC: &str = "application/vnd.google-apps.document";
const MD: &str = "# Notes\n\nThe team decided to use CSV for the export format.\n";

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

/// Mutable state of the mock Drive.
struct Drive {
    doc1_modified: String,
    root_readable: bool,
}

type State = Arc<Mutex<Drive>>;

fn item(id: &str, name: &str, mime: &str, modified: &str) -> Value {
    json!({"id": id, "name": name, "mimeType": mime, "modifiedTime": modified,
           "createdTime": "2026-08-01T00:00:00Z",
           "webViewLink": format!("https://docs.google.com/document/d/{id}/edit")})
}

fn list(items: Vec<Value>) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"files": items}))
}

async fn mock(state: State) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "at", "expires_in": 3600})),
        )
        .mount(&server)
        .await;
    // The root folder: readable or gone.
    let st = state.clone();
    Mock::given(path("/drive/v3/files/ROOT"))
        .respond_with(move |_: &Request| {
            if st.lock().unwrap().root_readable {
                ResponseTemplate::new(200).set_body_json(item(
                    "ROOT",
                    "Minutes",
                    FOLDER,
                    "2026-09-01T00:00:00Z",
                ))
            } else {
                ResponseTemplate::new(404)
            }
        })
        .mount(&server)
        .await;
    // Children of ROOT: a Doc, an old Doc, an image, a form, a shortcut to a Doc,
    // a shortcut to a folder, a lock file and a subfolder.
    let st = state.clone();
    Mock::given(path("/drive/v3/files"))
        .and(query_param("q", "'ROOT' in parents and trashed = false"))
        .respond_with(move |_: &Request| {
            let m = st.lock().unwrap().doc1_modified.clone();
            let mut short = item(
                "SHORT",
                "Link",
                "application/vnd.google-apps.shortcut",
                "2026-09-02T00:00:00Z",
            );
            short["shortcutDetails"] = json!({"targetId": "DOC2", "targetMimeType": DOC});
            let mut short_dir = item(
                "SHORTDIR",
                "Link to folder",
                "application/vnd.google-apps.shortcut",
                "2026-09-02T00:00:00Z",
            );
            short_dir["shortcutDetails"] = json!({"targetId": "SUB", "targetMimeType": FOLDER});
            list(vec![
                item("DOC1", "Plan", DOC, &m),
                item("OLD1", "Ancient", DOC, "2025-01-01T00:00:00Z"),
                item("IMG1", "photo.png", "image/png", "2026-09-05T00:00:00Z"),
                item(
                    "FORM1",
                    "Survey",
                    "application/vnd.google-apps.form",
                    "2026-09-05T00:00:00Z",
                ),
                item(
                    "LOCK1",
                    "~$Plan.docx",
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                    "2026-09-05T00:00:00Z",
                ),
                short,
                short_dir,
                item("SUB", "2026", FOLDER, "2026-09-01T00:00:00Z"),
            ])
        })
        .mount(&server)
        .await;
    Mock::given(path("/drive/v3/files"))
        .and(query_param("q", "'SUB' in parents and trashed = false"))
        .respond_with(list(vec![item(
            "DOC3",
            "Q3 minutes",
            DOC,
            "2026-09-08T00:00:00Z",
        )]))
        .mount(&server)
        .await;
    // The files themselves (metadata and export).
    let st = state.clone();
    Mock::given(path("/drive/v3/files/DOC1"))
        .respond_with(move |_: &Request| {
            let m = st.lock().unwrap().doc1_modified.clone();
            ResponseTemplate::new(200).set_body_json(item("DOC1", "Plan", DOC, &m))
        })
        .mount(&server)
        .await;
    for (id, name, m) in [
        ("DOC2", "Target doc", "2026-09-03T00:00:00Z"),
        ("DOC3", "Q3 minutes", "2026-09-08T00:00:00Z"),
        ("OLD1", "Ancient", "2025-01-01T00:00:00Z"),
    ] {
        Mock::given(path(format!("/drive/v3/files/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(item(id, name, DOC, m)))
            .mount(&server)
            .await;
    }
    for id in ["DOC1", "DOC2", "DOC3", "OLD1"] {
        Mock::given(path(format!("/drive/v3/files/{id}/export")))
            .and(query_param("mimeType", "text/markdown"))
            .respond_with(ResponseTemplate::new(200).set_body_string(MD))
            .mount(&server)
            .await;
    }
    server
}

struct Env {
    _dir: tempfile::TempDir,
    p: Pipeline,
    server: MockServer,
    state: State,
    clock: Arc<FixedClock>,
}

async fn env(folder: Value) -> Env {
    let state: State = Arc::new(Mutex::new(Drive {
        doc1_modified: "2026-09-05T00:00:00Z".into(),
        root_readable: true,
    }));
    let server = mock(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let cat = Catalog::create(&home).unwrap();
    let mut config = second_brain_google::default_config_json(&["docs".into()]);
    let mut f = json!({"key": "ROOT", "label": "Minutes", "url": "https://drive.google.com/drive/folders/ROOT"});
    f.as_object_mut()
        .unwrap()
        .extend(folder.as_object().cloned().unwrap_or_default());
    config["folders"] = json!([f]);
    cat.add_account(
        &AccountId::new("work").unwrap(),
        AccountKind::Google,
        "Work",
        Some("work@example.test"),
        &config,
    )
    .unwrap();
    drop(cat);
    let clock = Arc::new(FixedClock::new(
        second_brain_kernel::util::parse_ts("2026-09-10T00:00:00Z").unwrap(),
    ));
    let p = Pipeline::new(
        Catalog::open_with_clock(&home, clock.clone()).unwrap(),
        Arc::new(Factory(server.uri())),
    );
    Env {
        _dir: dir,
        p,
        server,
        state,
        clock,
    }
}

fn opts() -> SyncOptions {
    SyncOptions {
        no_summary: true,
        ..Default::default()
    }
}

async fn requests_to(s: &MockServer, p: &str) -> usize {
    s.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == p)
        .count()
}

fn entry(e: &Env, id: &str) -> Option<second_brain_store::Entry> {
    e.p.catalog()
        .entry_by_key("work", SourceKind::GoogleDoc, id)
        .unwrap()
}

#[tokio::test]
async fn files_of_the_folder_tree_become_entries_with_their_folder_path() {
    let e = env(json!({})).await;
    let r = e.p.sync(&opts()).await.unwrap();
    assert!(r.stats.errors.is_empty(), "{:?}", r.stats.errors);
    assert_eq!(r.status, Some(RunStatus::Ok));

    // The Doc, the shortcut's target and the Doc in the subfolder; not the old
    // Doc (outside the 30-day window), the image, the form, the lock file, or
    // the shortcut to a folder.
    for id in ["DOC1", "DOC2", "DOC3"] {
        let en = entry(&e, id).unwrap_or_else(|| panic!("{id} is missing"));
        assert_eq!(en.origin, EntryOrigin::Sync);
    }
    for id in ["OLD1", "IMG1", "FORM1", "LOCK1", "SHORT", "SHORTDIR", "SUB"] {
        assert!(entry(&e, id).is_none(), "{id}");
    }
    let m = |id: &str| entry(&e, id).unwrap().metadata;
    assert_eq!(m("DOC1")["folder_path"], "Minutes");
    assert_eq!(m("DOC1")["sync_folder"], "ROOT");
    assert_eq!(m("DOC3")["folder_path"], "Minutes / 2026");
    // The path becomes the summary's `location`, nothing else.
    assert!(m("DOC1").get("link_context").is_none());

    // Coverage is recorded.
    let c =
        e.p.catalog()
            .cursor(
                &AccountId::new("work").unwrap(),
                SourceKind::GoogleDoc,
                "folder:ROOT",
            )
            .unwrap()
            .unwrap();
    assert_eq!(c["covered_since"], "2026-08-11T00:00:00Z");
}

#[tokio::test]
async fn a_second_run_fetches_only_what_changed() {
    let e = env(json!({})).await;
    e.p.sync(&opts()).await.unwrap();
    assert_eq!(
        requests_to(&e.server, "/drive/v3/files/DOC1/export").await,
        1
    );
    let r = e.p.sync(&opts()).await.unwrap();
    assert!(r.stats.errors.is_empty(), "{:?}", r.stats.errors);
    assert_eq!(
        requests_to(&e.server, "/drive/v3/files/DOC1/export").await,
        1,
        "nothing changed"
    );
    assert_eq!(
        r.stats
            .sources
            .get("work/google.doc")
            .map_or(0, |s| s.new + s.updated),
        0
    );

    // DOC1 is edited.
    e.state.lock().unwrap().doc1_modified = "2026-09-09T12:00:00Z".into();
    let r = e.p.sync(&opts()).await.unwrap();
    assert_eq!(
        r.stats.sources["work/google.doc"].updated, 1,
        "{:?}",
        r.stats
    );
    assert_eq!(
        requests_to(&e.server, "/drive/v3/files/DOC1/export").await,
        2
    );
    assert_eq!(
        requests_to(&e.server, "/drive/v3/files/DOC3/export").await,
        1
    );
}

#[tokio::test]
async fn since_brings_in_older_files_and_include_narrows() {
    let e = env(json!({})).await;
    e.p.sync(&opts()).await.unwrap();
    assert!(entry(&e, "OLD1").is_none());
    let o = SyncOptions {
        since: second_brain_kernel::util::parse_ts("2024-12-01T00:00:00Z"),
        ..opts()
    };
    e.p.sync(&o).await.unwrap();
    assert!(entry(&e, "OLD1").is_some());

    let e = env(json!({"include": ["2026/**"]})).await;
    e.p.sync(&opts()).await.unwrap();
    assert!(entry(&e, "DOC3").is_some());
    assert!(entry(&e, "DOC1").is_none());
}

#[tokio::test]
async fn an_unreadable_folder_is_an_issue_that_resolves_when_it_works_again() {
    let e = env(json!({})).await;
    e.p.sync(&opts()).await.unwrap();
    e.state.lock().unwrap().root_readable = false;
    let r = e.p.sync(&opts()).await.unwrap();
    assert_eq!(r.status, Some(RunStatus::Partial));
    assert!(
        r.stats.errors.iter().any(|m| m.contains("Minutes")),
        "{:?}",
        r.stats.errors
    );
    let open = e.p.catalog().open_issues().unwrap();
    assert!(
        open.iter().any(|i| i.code == "folder.unavailable"),
        "{open:?}"
    );
    // The entries stay.
    assert!(entry(&e, "DOC1").is_some());

    e.state.lock().unwrap().root_readable = true;
    // Later runs start later; an issue not seen since the run began is over.
    e.clock.advance(chrono::Duration::hours(1));
    let r = e.p.sync(&opts()).await.unwrap();
    assert_eq!(r.status, Some(RunStatus::Ok), "{:?}", r.stats.errors);
    assert!(e.p.catalog().open_issues().unwrap().is_empty());
}

#[tokio::test]
async fn a_meet_notes_document_is_not_entered_twice() {
    let e = env(json!({})).await;
    e.p.catalog()
        .upsert_entry(&EntryUpdate {
            source_ref: SourceRef {
                account_id: AccountId::new("work").unwrap(),
                source_kind: SourceKind::GoogleMeet,
                source_id: "DOC3".into(),
                source_url: None,
                created_at: None,
                updated_at: None,
            },
            origin: EntryOrigin::Sync,
            raw: None,
            fetch_state: None,
            metadata: None,
            normalized: None,
        })
        .unwrap();
    e.p.sync(&opts()).await.unwrap();
    assert!(entry(&e, "DOC3").is_none());
    assert!(entry(&e, "DOC1").is_some());
}

#[tokio::test]
async fn a_source_filter_selects_folder_sync() {
    let e = env(json!({})).await;
    let o = SyncOptions {
        sources: vec![SourceKind::GoogleMeet],
        ..opts()
    };
    e.p.sync(&o).await.unwrap();
    assert!(
        entry(&e, "DOC1").is_none(),
        "--source google.meet must not list folders"
    );
    let o = SyncOptions {
        sources: vec![SourceKind::GoogleDoc],
        ..opts()
    };
    e.p.sync(&o).await.unwrap();
    assert!(entry(&e, "DOC1").is_some());
}
