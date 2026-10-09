//! google.doc ingest through the pipeline against mock Google APIs.
//! All names, IDs and texts are synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use sb_google::{GoogleApi, GoogleSource, OAuthClient, TokenProvider};
use sb_pipeline::ingest::{IngestOptions, IngestStatus};
use sb_pipeline::{Pipeline, PipelineError, SourceFactory};
use sb_store::{Account, Catalog, Home};
use second_brain_kernel::clock::FixedClock;
use second_brain_kernel::document::IngestSettings;
use second_brain_kernel::source::Source;
use second_brain_kernel::{
    AccountId, AccountKind, RawRole, RawStatus, Secret, SectionKind, SourceKind, SummaryStatus,
};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DOC_MD: &str = "# 設計メモ\n\n決定事項は \\[CSV\\] 形式で進めること。\n\n\n\n担当は Alice。 [00:01:02](#heading=h.1)\n";

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

fn meta(id: &str, name: &str, mime: &str) -> serde_json::Value {
    json!({"id": id, "name": name, "mimeType": mime,
           "createdTime": "2026-09-01T00:00:00Z", "modifiedTime": "2026-09-02T00:00:00Z",
           "webViewLink": format!("https://docs.google.com/document/d/{id}/edit"),
           "owners": [{"displayName": "Alice Example", "emailAddress": "alice@example.test"}]})
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
    // A native Doc.
    Mock::given(path("/drive/v3/files/DOC1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(meta(
            "DOC1",
            "Design memo",
            "application/vnd.google-apps.document",
        )))
        .mount(&server)
        .await;
    Mock::given(path("/drive/v3/files/DOC1/export"))
        .and(query_param("mimeType", "text/markdown"))
        .respond_with(ResponseTemplate::new(200).set_body_string(DOC_MD))
        .mount(&server)
        .await;
    // A text file in Drive (download).
    Mock::given(path("/drive/v3/files/TXT1"))
        .and(query_param("alt", "media"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("Plain notes about the budget review."),
        )
        .mount(&server)
        .await;
    Mock::given(path("/drive/v3/files/TXT1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(meta(
            "TXT1",
            "budget.txt",
            "text/plain",
        )))
        .mount(&server)
        .await;
    // A form: not supported.
    Mock::given(path("/drive/v3/files/FORM1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(meta(
            "FORM1",
            "Survey",
            "application/vnd.google-apps.form",
        )))
        .mount(&server)
        .await;
    // Trashed, forbidden, missing.
    let mut trashed = meta("TRASH1", "Old", "application/vnd.google-apps.document");
    trashed["trashed"] = json!(true);
    Mock::given(path("/drive/v3/files/TRASH1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(trashed))
        .mount(&server)
        .await;
    Mock::given(path("/drive/v3/files/DENIED1"))
        .respond_with(ResponseTemplate::new(403).set_body_json(
            json!({"error": {"message": "The caller does not have permission", "errors": [{"reason": "forbidden"}]}}),
        ))
        .mount(&server)
        .await;
    // A shortcut to DOC1.
    let mut shortcut = meta(
        "SHORT1",
        "Link to memo",
        "application/vnd.google-apps.shortcut",
    );
    shortcut["shortcutDetails"] = json!({"targetId": "DOC1"});
    Mock::given(path("/drive/v3/files/SHORT1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(shortcut))
        .mount(&server)
        .await;
    // A Doc that is too large to export.
    Mock::given(path("/drive/v3/files/BIG1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(meta(
            "BIG1",
            "Huge",
            "application/vnd.google-apps.document",
        )))
        .mount(&server)
        .await;
    Mock::given(path("/drive/v3/files/BIG1/export"))
        .respond_with(ResponseTemplate::new(403).set_body_json(
            json!({"error": {"message": "This file is too large to be exported.", "errors": [{"reason": "exportSizeLimitExceeded"}]}}),
        ))
        .mount(&server)
        .await;
    server
}

fn setup(server: &MockServer, accounts: &[&str]) -> (tempfile::TempDir, Pipeline) {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let cat = Catalog::create(&home).unwrap();
    for a in accounts {
        cat.add_account(
            &AccountId::new(*a).unwrap(),
            AccountKind::Google,
            a,
            Some(&format!("{a}@example.test")),
            &sb_google::default_config_json(&["docs".into()]),
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
    (dir, p)
}

fn opts() -> IngestOptions {
    IngestOptions {
        no_summary: true,
        ..Default::default()
    }
}

fn url(id: &str) -> String {
    format!("https://docs.google.com/document/d/{id}/edit")
}

async fn exports(s: &MockServer) -> usize {
    s.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/drive/v3/files/DOC1/export")
        .count()
}

#[tokio::test]
async fn native_doc_is_exported_cleaned_and_stored_as_text() {
    let server = mock().await;
    let (_d, p) = setup(&server, &["work"]);
    let o = IngestOptions {
        context: Some("Q3 launch".into()),
        ..opts()
    };
    let r = p.ingest(&[url("DOC1")], &o).await.unwrap();
    assert_eq!(
        r.results[0].status,
        IngestStatus::Created,
        "{:?}",
        r.results
    );
    let cat = p.catalog();
    let e = cat
        .entry_by_key("work", SourceKind::GoogleDoc, "DOC1")
        .unwrap()
        .unwrap();
    assert_eq!(e.title, "設計メモ");
    assert_eq!(e.raw_status, RawStatus::Present);
    assert_eq!(e.metadata["drive_file_id"], "DOC1");
    assert_eq!(e.metadata["export_format"], "markdown");
    assert_eq!(e.metadata["owners"][0]["name"], "Alice Example");
    assert_eq!(
        e.source_created_at,
        second_brain_kernel::util::parse_ts("2026-09-01T00:00:00Z")
    );
    assert_eq!(
        e.fetch_state.as_ref().unwrap()["modified_time"],
        "2026-09-02T00:00:00Z"
    );
    let secs = cat.sections(e.id).unwrap();
    let details = secs
        .iter()
        .find(|s| s.kind == SectionKind::Details)
        .unwrap();
    assert!(details.text.contains("[CSV]") && !details.text.contains("\\["));
    assert!(details.text.contains("00:01:02") && !details.text.contains("#heading"));
    assert!(!details.text.contains("\n\n\n"));
    let bg = secs
        .iter()
        .find(|s| s.kind == SectionKind::Background)
        .unwrap();
    assert_eq!(bg.text, "Q3 launch");
    // Only the extracted text is stored by default.
    let rows = cat.raw_objects(e.id).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].role, RawRole::ExtractedText);
}

#[tokio::test]
async fn unchanged_skips_the_export_and_keep_original_fetches_again() {
    let server = mock().await;
    let (_d, p) = setup(&server, &["work"]);
    p.ingest(&[url("DOC1")], &opts()).await.unwrap();
    assert_eq!(exports(&server).await, 1);
    let r = p.ingest(&[url("DOC1")], &opts()).await.unwrap();
    assert_eq!(r.results[0].status, IngestStatus::Unchanged);
    assert_eq!(exports(&server).await, 1);
    // A new context alone is a metadata-only update.
    let o = IngestOptions {
        context: Some("new reason".into()),
        ..opts()
    };
    let r = p.ingest(&[url("DOC1")], &o).await.unwrap();
    assert_eq!(r.results[0].status, IngestStatus::Updated);
    assert_eq!(exports(&server).await, 1);
    // Asking for the original needs a fetch; the export is stored unmodified.
    let o = IngestOptions {
        keep_original: true,
        ..opts()
    };
    let r = p.ingest(&[url("DOC1")], &o).await.unwrap();
    assert_eq!(r.results[0].status, IngestStatus::Updated);
    assert_eq!(exports(&server).await, 2);
    let cat = p.catalog();
    let e = cat
        .entry_by_key("work", SourceKind::GoogleDoc, "DOC1")
        .unwrap()
        .unwrap();
    let rows = cat.raw_objects(e.id).unwrap();
    assert_eq!(rows.len(), 2);
    let primary = rows.iter().find(|r| r.role == RawRole::Primary).unwrap();
    let bytes = std::fs::read(p.home().root().join(&primary.path)).unwrap();
    assert_eq!(String::from_utf8(bytes).unwrap(), DOC_MD);
    // The context survives the refetch.
    let secs = cat.sections(e.id).unwrap();
    assert_eq!(
        secs.iter()
            .find(|s| s.kind == SectionKind::Background)
            .unwrap()
            .text,
        "new reason"
    );
}

#[tokio::test]
async fn drive_text_files_unsupported_types_and_failures() {
    let server = mock().await;
    let (_d, p) = setup(&server, &["work"]);
    let ids = ["TXT1", "FORM1", "TRASH1", "DENIED1", "NOPE1", "BIG1"];
    let locators: Vec<String> = ids.iter().map(|i| url(i)).collect();
    let r = p.ingest(&locators, &opts()).await.unwrap();
    let st: Vec<IngestStatus> = r.results.iter().map(|x| x.status).collect();
    assert_eq!(
        st,
        vec![
            IngestStatus::Created,
            IngestStatus::NotApplicable,
            IngestStatus::Failed,
            IngestStatus::Failed,
            IngestStatus::Failed,
            IngestStatus::Failed
        ],
        "{:#?}",
        r.results
    );
    assert!(
        r.results[1]
            .message
            .as_deref()
            .unwrap()
            .contains("unsupported Drive file type")
    );
    assert!(r.results[2].message.as_deref().unwrap().contains("trash"));
    assert!(
        r.results[3]
            .message
            .as_deref()
            .unwrap()
            .contains("no access")
    );
    assert!(r.results[5].message.as_deref().unwrap().contains("10 MB"));
    let cat = p.catalog();
    let t = cat
        .entry_by_key("work", SourceKind::GoogleDoc, "TXT1")
        .unwrap()
        .unwrap();
    assert_eq!(t.title, "budget.txt");
    // Shorter than summary.min_chars: kept as extracted text, not summarized.
    assert_eq!(t.summary_status, SummaryStatus::Skipped);
    assert!(
        r.results[0]
            .message
            .as_deref()
            .unwrap()
            .contains("summary.min_chars")
    );
    assert!(r.has_problems());
}

#[tokio::test]
async fn shortcut_is_followed_to_its_target() {
    let server = mock().await;
    let (_d, p) = setup(&server, &["work"]);
    let r = p.ingest(&[url("SHORT1")], &opts()).await.unwrap();
    assert_eq!(
        r.results[0].status,
        IngestStatus::Created,
        "{:?}",
        r.results
    );
    // The target is now known: the same shortcut updates it instead of creating.
    let r2 = p.ingest(&[url("SHORT1")], &opts()).await.unwrap();
    assert_eq!(
        r2.results[0].status,
        IngestStatus::Updated,
        "{:?}",
        r2.results
    );
    let cat = p.catalog();
    assert!(
        cat.entry_by_key("work", SourceKind::GoogleDoc, "DOC1")
            .unwrap()
            .is_some()
    );
    assert!(
        cat.entry_by_key("work", SourceKind::GoogleDoc, "SHORT1")
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn gemini_notes_are_a_duplicate_and_dry_run_writes_nothing() {
    let server = mock().await;
    let (_d, p) = setup(&server, &["work"]);
    // The same Drive file already known as a Meet entry.
    {
        let cat = p.catalog();
        let sref = second_brain_kernel::SourceRef {
            account_id: AccountId::new("work").unwrap(),
            source_kind: SourceKind::GoogleMeet,
            source_id: "DOC1".into(),
            source_url: None,
            created_at: None,
            updated_at: None,
        };
        cat.upsert_entry(&sb_store::EntryUpdate {
            source_ref: sref,
            origin: second_brain_kernel::EntryOrigin::Sync,
            raw: None,
            fetch_state: None,
            metadata: Some(json!({})),
            normalized: None,
        })
        .unwrap();
    }
    let r = p.ingest(&[url("DOC1")], &opts()).await.unwrap();
    assert_eq!(r.results[0].status, IngestStatus::Duplicate);
    assert!(r.results[0].duplicate_of.is_some());
    // Dry run: classification and lookups only.
    let o = IngestOptions {
        dry_run: true,
        ..opts()
    };
    let r = p.ingest(&[url("TXT1")], &o).await.unwrap();
    assert_eq!(r.results[0].status, IngestStatus::WouldCreate);
    let reqs = server.received_requests().await.unwrap();
    assert!(reqs.iter().all(|q| !q.url.path().contains("TXT1")));
    assert!(
        p.catalog()
            .entry_by_key("work", SourceKind::GoogleDoc, "TXT1")
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn account_is_probed_and_the_first_that_can_read_wins() {
    let server = mock().await;
    let (_d, p) = setup(&server, &["alpha", "beta"]);
    // Both accounts share the mock, so both can read; the first by creation wins.
    let r = p.ingest(&[url("DOC1")], &opts()).await.unwrap();
    assert_eq!(r.results[0].account.as_deref(), Some("alpha"));
    // Explicit account.
    let o = IngestOptions {
        account: Some("beta".into()),
        ..opts()
    };
    let r = p.ingest(&[url("TXT1")], &o).await.unwrap();
    assert_eq!(r.results[0].account.as_deref(), Some("beta"));
    // A re-ingest goes to the account that already holds the entry.
    let r = p.ingest(&[url("TXT1")], &opts()).await.unwrap();
    assert_eq!(r.results[0].status, IngestStatus::Unchanged);
    assert_eq!(r.results[0].account.as_deref(), Some("beta"));
    // Probing reports every account when none can read the file.
    let r = p.ingest(&[url("NOPE1")], &opts()).await.unwrap();
    let msg = r.results[0].message.clone().unwrap();
    assert!(msg.contains("alpha") && msg.contains("beta"), "{msg}");
}

#[tokio::test]
async fn no_google_account_is_a_clear_failure() {
    let server = mock().await;
    let (_d, p) = setup(&server, &[]);
    let r = p.ingest(&[url("DOC1")], &opts()).await.unwrap();
    assert_eq!(r.results[0].status, IngestStatus::Failed);
    assert!(
        r.results[0]
            .message
            .as_deref()
            .unwrap()
            .contains("sb account add google")
    );
}
