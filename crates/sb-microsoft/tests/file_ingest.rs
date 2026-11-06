//! microsoft.file ingest through the pipeline against mock Entra and Graph
//! endpoints. All names, IDs and texts are synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use second_brain_kernel::clock::FixedClock;
use second_brain_kernel::document::IngestSettings;
use second_brain_kernel::source::Source;
use second_brain_kernel::{
    AccountId, AccountKind, AccountStatus, RawRole, RawStatus, Secret, SourceKind,
};
use second_brain_microsoft::graph::share_id;
use second_brain_microsoft::{
    GraphApi, MicrosoftClient, MicrosoftConfig, MicrosoftSource, RefreshSink, TokenProvider,
};
use second_brain_pipeline::ingest::{IngestOptions, IngestStatus};
use second_brain_pipeline::{Pipeline, PipelineError, SourceFactory};
use second_brain_store::{Account, Catalog, Home};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CID: &str = "11111111-2222-3333-4444-555555555555";
const URL: &str = "https://contoso.sharepoint.com/sites/x/Shared%20Documents/notes.md";
const TEXT: &str = "# Launch plan\n\nThe decision is to ship the beta in March. Owner: Alice.\n";

struct Factory {
    base: String,
    allow_http: bool,
    max_bytes: Option<u64>,
}

impl SourceFactory for Factory {
    fn source(
        &self,
        account: &Account,
        _c: &Catalog,
    ) -> Result<Option<Arc<dyn Source>>, PipelineError> {
        let client = MicrosoftClient::new(CID, "common").unwrap().with_endpoints(
            format!("{}/auth", self.base),
            format!("{}/token", self.base),
        );
        let sink: RefreshSink = Arc::new(|_| Ok(()));
        let tokens = Arc::new(TokenProvider::new(
            client,
            Secret::new(format!("rt-{}", account.id)),
            sink,
        ));
        let mut api = GraphApi::new(tokens)?.with_base(&self.base);
        if self.allow_http {
            api = api.allow_http_downloads()?;
        }
        let mut settings = IngestSettings::default();
        if let Some(m) = self.max_bytes {
            settings.max_file_bytes = m;
        }
        Ok(Some(Arc::new(MicrosoftSource::new(
            account.ctx(),
            Some(api),
            settings,
        ))))
    }
}

fn item(id: &str, name: &str, etag: &str, size: usize, extra: Value) -> Value {
    let mut v = json!({
        "id": id, "name": name, "size": size, "eTag": etag,
        "webUrl": format!("https://contoso.sharepoint.com/sites/x/Shared%20Documents/{name}"),
        "createdDateTime": "2026-09-01T00:00:00Z",
        "lastModifiedDateTime": "2026-09-02T00:00:00Z",
        "file": {"mimeType": "text/markdown"},
        "parentReference": {"driveId": "b!DRIVE", "driveType": "documentLibrary"},
        "createdBy": {"user": {"displayName": "Alice Example", "email": "alice@example.test"}},
    });
    for (k, x) in extra.as_object().cloned().unwrap_or_default() {
        v[k] = x;
    }
    v
}

/// Token endpoint: every refresh token gets the access token `at-<account>`.
async fn mount_tokens(s: &MockServer, accounts: &[&str]) {
    for a in accounts {
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains(format!("refresh_token=rt-{a}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": format!("at-{a}"), "expires_in": 3600
            })))
            .mount(s)
            .await;
    }
}

/// `shares` and `items` answer with `it`; the content redirects to `/dl/<id>`.
async fn mount_item(s: &MockServer, url: &str, it: &Value, body: &str) {
    let id = it["id"].as_str().unwrap();
    let drive = it["parentReference"]["driveId"].as_str().unwrap();
    Mock::given(path(format!("/shares/{}/driveItem", share_id(url))))
        .respond_with(ResponseTemplate::new(200).set_body_json(it))
        .mount(s)
        .await;
    Mock::given(path(format!("/drives/{drive}/items/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(it))
        .mount(s)
        .await;
    Mock::given(path(format!("/drives/{drive}/items/{id}/content")))
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

fn setup_with(server: &MockServer, accounts: &[&str], f: Factory) -> (tempfile::TempDir, Pipeline) {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let cat = Catalog::create(&home).unwrap();
    for a in accounts {
        cat.add_account(
            &AccountId::new(*a).unwrap(),
            AccountKind::Microsoft,
            a,
            Some("tid:oid"),
            &MicrosoftConfig::new(CID, "common").to_value(),
        )
        .unwrap();
    }
    drop(cat);
    let _ = server;
    let clock = Arc::new(FixedClock::new(
        second_brain_kernel::util::parse_ts("2026-09-10T00:00:00Z").unwrap(),
    ));
    let p = Pipeline::new(Catalog::open_with_clock(&home, clock).unwrap(), Arc::new(f));
    (dir, p)
}

fn setup(server: &MockServer, accounts: &[&str]) -> (tempfile::TempDir, Pipeline) {
    setup_with(
        server,
        accounts,
        Factory {
            base: server.uri(),
            allow_http: true,
            max_bytes: None,
        },
    )
}

fn opts() -> IngestOptions {
    IngestOptions {
        no_summary: true,
        ..Default::default()
    }
}

async fn hits(s: &MockServer, p: &str) -> usize {
    s.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == p)
        .count()
}

#[tokio::test]
async fn a_file_is_downloaded_extracted_and_stored_as_text() {
    let server = MockServer::start().await;
    mount_tokens(&server, &["work"]).await;
    mount_item(
        &server,
        URL,
        &item("ITEM1", "notes.md", "\"e1\"", TEXT.len(), json!({})),
        TEXT,
    )
    .await;
    let (_d, p) = setup(&server, &["work"]);
    let o = IngestOptions {
        context: Some("Beta launch".into()),
        ..opts()
    };
    let r = p.ingest(&[URL.to_string()], &o).await.unwrap();
    assert_eq!(
        r.results[0].status,
        IngestStatus::Created,
        "{:?}",
        r.results
    );
    assert_eq!(r.results[0].source_kind.as_deref(), Some("microsoft.file"));

    let cat = p.catalog();
    let e = cat
        .entry_by_key("work", SourceKind::MicrosoftFile, "b!DRIVE/ITEM1")
        .unwrap()
        .unwrap();
    assert_eq!(e.title, "Launch plan");
    assert_eq!(e.raw_status, RawStatus::Present);
    assert_eq!(e.metadata["drive_id"], "b!DRIVE");
    assert_eq!(e.metadata["item_id"], "ITEM1");
    assert_eq!(e.metadata["drive_type"], "documentLibrary");
    assert_eq!(e.metadata["owners"][0]["name"], "Alice Example");
    assert_eq!(e.metadata["context"], "Beta launch");
    assert_eq!(
        e.source_url.as_deref(),
        Some("https://contoso.sharepoint.com/sites/x/Shared%20Documents/notes.md")
    );
    // Only the extracted text is stored; the original is dropped.
    let roles: Vec<_> = cat
        .raw_objects(e.id)
        .unwrap()
        .iter()
        .map(|r| r.role)
        .collect();
    assert_eq!(roles, vec![RawRole::ExtractedText]);
}

#[tokio::test]
async fn the_download_carries_no_credentials() {
    let server = MockServer::start().await;
    mount_tokens(&server, &["work"]).await;
    mount_item(
        &server,
        URL,
        &item("ITEM1", "notes.md", "\"e1\"", TEXT.len(), json!({})),
        TEXT,
    )
    .await;
    let (_d, p) = setup(&server, &["work"]);
    p.ingest(&[URL.to_string()], &opts()).await.unwrap();
    let reqs = server.received_requests().await.unwrap();
    let dl = reqs.iter().find(|r| r.url.path() == "/dl/ITEM1").unwrap();
    assert!(
        dl.headers.get("authorization").is_none(),
        "the pre-authenticated URL must not receive the token"
    );
    let graph = reqs
        .iter()
        .find(|r| r.url.path().ends_with("/driveItem"))
        .unwrap();
    assert_eq!(
        graph
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        "Bearer at-work"
    );
}

#[tokio::test]
async fn an_http_download_url_is_refused() {
    let server = MockServer::start().await;
    mount_tokens(&server, &["work"]).await;
    mount_item(
        &server,
        URL,
        &item("ITEM1", "notes.md", "\"e1\"", TEXT.len(), json!({})),
        TEXT,
    )
    .await;
    // The mock server speaks http, so a client that requires https must refuse.
    let (_d, p) = setup_with(
        &server,
        &["work"],
        Factory {
            base: server.uri(),
            allow_http: false,
            max_bytes: None,
        },
    );
    let r = p.ingest(&[URL.to_string()], &opts()).await.unwrap();
    assert_eq!(r.results[0].status, IngestStatus::Failed);
    assert!(
        r.results[0]
            .message
            .as_deref()
            .unwrap()
            .contains("not https"),
        "{:?}",
        r.results
    );
    assert_eq!(hits(&server, "/dl/ITEM1").await, 0);
}

#[tokio::test]
async fn reingest_is_unchanged_without_a_download_and_a_new_etag_updates() {
    let server = MockServer::start().await;
    mount_tokens(&server, &["work"]).await;
    mount_item(
        &server,
        URL,
        &item("ITEM1", "notes.md", "\"e1\"", TEXT.len(), json!({})),
        TEXT,
    )
    .await;
    let (_d, p) = setup(&server, &["work"]);
    p.ingest(&[URL.to_string()], &opts()).await.unwrap();
    assert_eq!(hits(&server, "/dl/ITEM1").await, 1);

    let r = p.ingest(&[URL.to_string()], &opts()).await.unwrap();
    assert_eq!(
        r.results[0].status,
        IngestStatus::Unchanged,
        "{:?}",
        r.results
    );
    assert_eq!(hits(&server, "/dl/ITEM1").await, 1, "no second download");

    server.reset().await;
    mount_tokens(&server, &["work"]).await;
    let new_text = format!("{TEXT}\nUpdate: the date moved to April.\n");
    mount_item(
        &server,
        URL,
        &item("ITEM1", "notes.md", "\"e2\"", new_text.len(), json!({})),
        &new_text,
    )
    .await;
    let r = p.ingest(&[URL.to_string()], &opts()).await.unwrap();
    assert_eq!(
        r.results[0].status,
        IngestStatus::Updated,
        "{:?}",
        r.results
    );
    let cat = p.catalog();
    let e = cat
        .entry_by_key("work", SourceKind::MicrosoftFile, "b!DRIVE/ITEM1")
        .unwrap()
        .unwrap();
    assert_eq!(e.metadata["ms_etag"], "\"e2\"");
}

#[tokio::test]
async fn a_remote_item_is_keyed_by_its_target() {
    let server = MockServer::start().await;
    mount_tokens(&server, &["work"]).await;
    let target = item("TARGET1", "shared.md", "\"t1\"", TEXT.len(), json!({}));
    let target_drive = json!({"driveId": "b!THEIRS", "driveType": "business"});
    let mut target = target;
    target["parentReference"] = target_drive;
    // `shares` answers with the item in the user's drive pointing at the target.
    let shortcut = json!({
        "id": "LOCAL1", "name": "shared.md", "parentReference": {"driveId": "b!MINE"},
        "remoteItem": target.clone()
    });
    Mock::given(path(format!("/shares/{}/driveItem", share_id(URL))))
        .respond_with(ResponseTemplate::new(200).set_body_json(&shortcut))
        .mount(&server)
        .await;
    // The target is fetched by its own IDs.
    mount_item(&server, "https://other.example/unused", &target, TEXT).await;
    let (_d, p) = setup(&server, &["work"]);
    let r = p.ingest(&[URL.to_string()], &opts()).await.unwrap();
    assert_eq!(
        r.results[0].status,
        IngestStatus::Created,
        "{:?}",
        r.results
    );
    assert!(
        p.catalog()
            .entry_by_key("work", SourceKind::MicrosoftFile, "b!THEIRS/TARGET1")
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn folders_notebooks_and_unsupported_types_are_not_applicable() {
    let server = MockServer::start().await;
    mount_tokens(&server, &["work"]).await;
    let cases = [
        (
            "https://contoso.sharepoint.com/sites/x/Shared%20Documents/Folder",
            item(
                "F1",
                "Folder",
                "\"f\"",
                0,
                json!({"folder": {"childCount": 3}}),
            ),
            "folders",
        ),
        (
            "https://contoso.sharepoint.com/:o:/s/x/NotebookLink",
            item(
                "N1",
                "Notebook",
                "\"n\"",
                0,
                json!({"package": {"type": "oneNote"}}),
            ),
            "package",
        ),
        (
            "https://contoso.sharepoint.com/sites/x/Shared%20Documents/old.doc",
            item("D1", "old.doc", "\"d\"", 10, json!({})),
            "save it as docx",
        ),
        (
            "https://contoso.sharepoint.com/sites/x/Shared%20Documents/clip.mp4",
            item("V1", "clip.mp4", "\"v\"", 10, json!({})),
            "unsupported file type",
        ),
    ];
    for (u, it, _) in &cases {
        mount_item(&server, u, it, "ignored").await;
    }
    let (_d, p) = setup(&server, &["work"]);
    for (u, _, why) in &cases {
        let r = p.ingest(&[u.to_string()], &opts()).await.unwrap();
        assert_eq!(
            r.results[0].status,
            IngestStatus::NotApplicable,
            "{u}: {:?}",
            r.results
        );
        assert!(
            r.results[0].message.as_deref().unwrap().contains(why),
            "{u}: {:?}",
            r.results
        );
    }
    assert_eq!(
        hits(&server, "/dl/D1").await + hits(&server, "/dl/V1").await,
        0
    );
}

#[tokio::test]
async fn an_oversized_file_is_refused_before_the_download() {
    let server = MockServer::start().await;
    mount_tokens(&server, &["work"]).await;
    mount_item(
        &server,
        URL,
        &item("BIG1", "notes.md", "\"e\"", 5000, json!({})),
        TEXT,
    )
    .await;
    let (_d, p) = setup_with(
        &server,
        &["work"],
        Factory {
            base: server.uri(),
            allow_http: true,
            max_bytes: Some(1000),
        },
    );
    let r = p.ingest(&[URL.to_string()], &opts()).await.unwrap();
    assert_eq!(r.results[0].status, IngestStatus::Failed);
    assert!(
        r.results[0]
            .message
            .as_deref()
            .unwrap()
            .contains("max_file_bytes"),
        "{:?}",
        r.results
    );
    assert_eq!(hits(&server, "/dl/BIG1").await, 0);
}

#[tokio::test]
async fn missing_items_say_so_and_a_bad_url_suggests_a_sharing_link() {
    let server = MockServer::start().await;
    mount_tokens(&server, &["work"]).await;
    let denied = "https://contoso.sharepoint.com/sites/x/Shared%20Documents/secret.md";
    let odd = "https://contoso.sharepoint.com/_layouts/15/Doc.aspx?sourcedoc=%7BABC%7D";
    Mock::given(path(format!("/shares/{}/driveItem", share_id(denied))))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(
                json!({"error": {"code": "accessDenied", "message": "Access denied"}}),
            ),
        )
        .mount(&server)
        .await;
    Mock::given(path(format!("/shares/{}/driveItem", share_id(odd))))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"error": {"code": "invalidRequest", "message": "Invalid sharing id"}}),
        ))
        .mount(&server)
        .await;
    let (_d, p) = setup(&server, &["work"]);
    let r = p
        .ingest(&[denied.to_string(), odd.to_string()], &opts())
        .await
        .unwrap();
    let msg = |i: usize| r.results[i].message.clone().unwrap();
    assert_eq!(r.results[0].status, IngestStatus::Failed);
    // Graph's own text is kept, so a missing permission is visible.
    assert!(
        msg(0).starts_with("work: not found or no access")
            && msg(0).contains("accessDenied: Access denied"),
        "{}",
        msg(0)
    );
    assert_eq!(r.results[1].status, IngestStatus::Failed);
    assert!(msg(1).contains("sharing link"), "{}", msg(1));
}

#[tokio::test]
async fn the_second_account_is_probed_when_the_first_cannot_read() {
    let server = MockServer::start().await;
    mount_tokens(&server, &["first", "second"]).await;
    let it = item("ITEM1", "notes.md", "\"e1\"", TEXT.len(), json!({}));
    // `first` is refused; `second` reads the item.
    Mock::given(path(format!("/shares/{}/driveItem", share_id(URL))))
        .and(header("authorization", "Bearer at-first"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({"error": {"code": "accessDenied", "message": "no"}})),
        )
        .mount(&server)
        .await;
    mount_item(&server, URL, &it, TEXT).await;
    let (_d, p) = setup(&server, &["first", "second"]);
    let r = p.ingest(&[URL.to_string()], &opts()).await.unwrap();
    assert_eq!(
        r.results[0].status,
        IngestStatus::Created,
        "{:?}",
        r.results
    );
    assert_eq!(r.results[0].account.as_deref(), Some("second"));
}

#[tokio::test]
async fn a_revoked_token_marks_the_account_for_reauthentication() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"error": "invalid_grant", "error_description": "AADSTS70008: expired"}),
        ))
        .mount(&server)
        .await;
    let (_d, p) = setup(&server, &["work"]);
    let r = p.ingest(&[URL.to_string()], &opts()).await.unwrap();
    assert_eq!(r.results[0].status, IngestStatus::Failed);
    assert!(
        r.results[0]
            .message
            .as_deref()
            .unwrap()
            .contains("sb auth login work"),
        "{:?}",
        r.results
    );
    let a = p.catalog().account_by_str("work").unwrap().unwrap();
    assert_eq!(a.status, AccountStatus::NeedsReauth);
}

#[tokio::test]
async fn throttling_is_retried_after_retry_after() {
    let server = MockServer::start().await;
    mount_tokens(&server, &["work"]).await;
    Mock::given(path(format!("/shares/{}/driveItem", share_id(URL))))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    mount_item(
        &server,
        URL,
        &item("ITEM1", "notes.md", "\"e1\"", TEXT.len(), json!({})),
        TEXT,
    )
    .await;
    let (_d, p) = setup(&server, &["work"]);
    let r = p.ingest(&[URL.to_string()], &opts()).await.unwrap();
    assert_eq!(
        r.results[0].status,
        IngestStatus::Created,
        "{:?}",
        r.results
    );
}

#[tokio::test]
async fn without_a_microsoft_account_the_hint_names_the_command() {
    let server = MockServer::start().await;
    let (_d, p) = setup(&server, &[]);
    let r = p.ingest(&[URL.to_string()], &opts()).await.unwrap();
    assert_eq!(r.results[0].status, IngestStatus::Failed);
    assert!(
        r.results[0]
            .message
            .as_deref()
            .unwrap()
            .contains("sb account add microsoft"),
        "{:?}",
        r.results
    );
}

#[tokio::test]
async fn a_dry_run_makes_no_request() {
    let server = MockServer::start().await;
    let (_d, p) = setup(&server, &["work"]);
    let o = IngestOptions {
        dry_run: true,
        ..opts()
    };
    let r = p.ingest(&[URL.to_string()], &o).await.unwrap();
    assert_eq!(
        r.results[0].status,
        IngestStatus::WouldCreate,
        "{:?}",
        r.results
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}
