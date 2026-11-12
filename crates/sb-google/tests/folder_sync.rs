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
    // A plain file, for "pass a folder" errors.
    Mock::given(path("/drive/v3/files/FILE1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(item(
            "FILE1",
            "Plan",
            DOC,
            "2026-09-01T00:00:00Z",
        )))
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
    env_inner(Some(folder)).await
}

async fn env_inner(folder: Option<Value>) -> Env {
    let state: State = Arc::new(Mutex::new(Drive {
        doc1_modified: "2026-09-05T00:00:00Z".into(),
        root_readable: true,
    }));
    let server = mock(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let cat = Catalog::create(&home).unwrap();
    let mut config = second_brain_google::default_config_json(&["docs".into()]);
    if let Some(folder) = folder {
        let mut f = json!({"key": "ROOT", "label": "Minutes", "url": "https://drive.google.com/drive/folders/ROOT"});
        f.as_object_mut()
            .unwrap()
            .extend(folder.as_object().cloned().unwrap_or_default());
        config["folders"] = json!([f]);
    }
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

const ROOT_URL: &str = "https://drive.google.com/drive/folders/ROOT?usp=sharing";

#[tokio::test]
async fn folder_add_resolves_the_url_and_stores_the_setting() {
    use second_brain_pipeline::folders::FolderAdd;
    let e = env_inner(None).await;
    let o = FolderAdd {
        include: vec!["*.docx".into()],
        max_depth: Some(3),
        ..Default::default()
    };
    let info = e.p.folder_add("work", ROOT_URL, &o).await.unwrap();
    assert_eq!(info.folder.key, "ROOT");
    assert_eq!(info.folder.label, "Minutes", "the folder's own name");
    assert_eq!(
        (info.folder.max_depth, info.folder.include.clone()),
        (3, vec!["*.docx".to_string()])
    );
    assert_eq!(info.covered_since, None);

    let listed = e.p.folder_list(None).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].folder.url, ROOT_URL);
    // Adding it again updates it; a label already taken by another folder is refused.
    let again = FolderAdd {
        label: Some("Team minutes".into()),
        ..Default::default()
    };
    let info = e.p.folder_add("work", ROOT_URL, &again).await.unwrap();
    assert_eq!(info.folder.label, "Team minutes");
    assert_eq!(info.folder.max_depth, 3, "unset options keep their values");
    assert_eq!(e.p.folder_list(Some("work")).unwrap().len(), 1);
    // Nothing was fetched or entered.
    assert!(entry(&e, "DOC1").is_none());
}

#[tokio::test]
async fn folder_add_refuses_what_is_not_a_folder() {
    use second_brain_pipeline::folders::FolderAdd;
    let e = env_inner(None).await;
    for (url, needle) in [
        (
            "https://drive.google.com/drive/folders/FILE1",
            "not a folder",
        ),
        (
            "https://docs.google.com/document/d/FILE1/edit",
            "not a Google Drive folder URL",
        ),
        ("https://example.test/x", "not a Google Drive folder URL"),
        ("https://drive.google.com/drive/folders/NOPE", "not found"),
    ] {
        let err =
            e.p.folder_add("work", url, &FolderAdd::default())
                .await
                .unwrap_err();
        assert!(matches!(err, PipelineError::Invalid(_)), "{err:?}");
        assert!(err.to_string().contains(needle), "{url}: {err}");
    }
    assert!(
        e.p.folder_add("nobody", ROOT_URL, &FolderAdd::default())
            .await
            .is_err()
    );
    assert!(e.p.folder_list(None).unwrap().is_empty());
}

#[tokio::test]
async fn folder_check_counts_without_writing_anything() {
    let e = env(json!({})).await;
    let c = e.p.folder_check(Some("work"), None, None).await.unwrap();
    assert_eq!(c.len(), 1);
    let r = &c[0].report;
    assert_eq!(
        (r.status.as_str(), r.in_scope, r.new, r.unchanged),
        ("ok", 3, 3, 0)
    );
    assert_eq!(c[0].window_from.as_deref(), Some("2026-08-11T00:00:00Z"));
    // Read-only: no queue rows, no coverage, no entries, no issues.
    assert!(e.p.catalog().queue(None, &[]).unwrap().is_empty());
    assert!(
        e.p.catalog()
            .cursor(
                &AccountId::new("work").unwrap(),
                SourceKind::GoogleDoc,
                "folder:ROOT"
            )
            .unwrap()
            .is_none()
    );
    assert!(entry(&e, "DOC1").is_none());

    // After a sync everything is known; --since reaches back to the old file.
    e.p.sync(&opts()).await.unwrap();
    let c =
        e.p.folder_check(Some("work"), Some("Minutes"), None)
            .await
            .unwrap();
    assert_eq!((c[0].report.new, c[0].report.unchanged), (0, 3));
    let since = second_brain_kernel::util::parse_ts("2024-12-01T00:00:00Z");
    let c = e.p.folder_check(None, None, since).await.unwrap();
    assert_eq!((c[0].report.new, c[0].report.unchanged), (1, 3));
    assert!(
        e.p.folder_check(Some("work"), Some("Nope"), None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn folder_list_shows_coverage_and_remove_keeps_the_entries() {
    let e = env(json!({})).await;
    assert_eq!(e.p.folder_list(None).unwrap()[0].covered_since, None);
    e.p.sync(&opts()).await.unwrap();
    let l = e.p.folder_list(None).unwrap();
    assert_eq!(l[0].covered_since.as_deref(), Some("2026-08-11T00:00:00Z"));
    assert!(l[0].listed_at.is_some());

    let gone = e.p.folder_remove("work", "ROOT").unwrap();
    assert_eq!(gone.label, "Minutes");
    assert!(e.p.folder_list(None).unwrap().is_empty());
    assert!(
        e.p.catalog()
            .cursor(
                &AccountId::new("work").unwrap(),
                SourceKind::GoogleDoc,
                "folder:ROOT"
            )
            .unwrap()
            .is_none()
    );
    assert!(entry(&e, "DOC1").is_some(), "entries stay");
    assert!(e.p.folder_remove("work", "ROOT").is_err());
    // With no folders the account is no longer synced for documents.
    assert!(
        e.p.folder_check(Some("work"), None, None)
            .await
            .unwrap()
            .is_empty()
    );
}
