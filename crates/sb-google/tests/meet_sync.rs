//! google.meet sync through the pipeline against mock Google APIs.
//! All names, IDs and texts are synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use sb_google::{GoogleApi, MeetSource, OAuthClient, TokenProvider};
use sb_pipeline::sync::SyncOptions;
use sb_pipeline::{Pipeline, PipelineError, SourceFactory};
use sb_store::{Account, Catalog, Home};
use second_brain_kernel::clock::FixedClock;
use second_brain_kernel::source::Source;
use second_brain_kernel::{
    AccountId, AccountKind, GeneratorKind, RawStatus, Secret, SectionKind, SourceKind,
    SummaryStatus,
};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const NOTES: &str = "# 📝 メモ\n\n## **Design Review**\n\n\
招待済み [Alice Example](mailto:alice@example.test) ~~[Bob Example](mailto:bob@example.test)~~\n\n\
### **概要**\n\nCSV エクスポートの設計をレビューした。\n\n\
### **決定事項**\n\n* CSV 形式で進める\n\n\
### **次のステップ**\n\n* [Alice Example] 仕様書を更新する\n\n\
### **詳細**\n\n* 詳細な議論 ([00:01:02](#heading=h.1))\n\n\
# **📖 文字起こし**\n\n**Alice Example:** 始めます。\n";

async fn google_mock(modified: Arc<Mutex<String>>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "at", "expires_in": 3600})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/calendar/v3/calendars/primary/events"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            {"id": "ev1", "status": "confirmed", "summary": "Design Review", "recurringEventId": "r1",
             "htmlLink": "https://calendar.google.com/calendar/event?eid=ev1",
             "start": {"dateTime": "2026-09-09T01:00:00Z"},
             "attendees": [{"email": "alice@example.test", "self": true, "responseStatus": "accepted"}],
             "attachments": [
                {"fileId": "NOTES1", "mimeType": "application/vnd.google-apps.document", "title": "Notes"},
                {"fileId": "AGENDA1", "mimeType": "application/vnd.google-apps.document", "title": "Agenda"},
                {"fileId": "PDF1", "mimeType": "application/pdf"}]},
            {"id": "ev2", "status": "confirmed", "summary": "Declined meeting",
             "start": {"dateTime": "2026-09-09T03:00:00Z"},
             "attendees": [{"email": "alice@example.test", "self": true, "responseStatus": "declined"}],
             "attachments": [{"fileId": "NOTES2", "mimeType": "application/vnd.google-apps.document"}]},
            {"id": "ev3", "status": "cancelled", "summary": "Cancelled"}
        ]})))
        .mount(&server)
        .await;
    // Drive strategy: no "Meet Recordings" folder in this account.
    Mock::given(path("/drive/v3/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"files": []})))
        .mount(&server)
        .await;
    let m = modified.clone();
    Mock::given(path("/drive/v3/files/NOTES1"))
        .respond_with(move |_: &Request| {
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "NOTES1", "name": "Design Review - Notes by Gemini",
                "mimeType": "application/vnd.google-apps.document",
                "createdTime": "2026-09-09T01:50:00Z", "modifiedTime": *m.lock().unwrap(), "parents": ["F1"]}))
        })
        .mount(&server)
        .await;
    Mock::given(path("/drive/v3/files/AGENDA1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "AGENDA1", "mimeType": "application/vnd.google-apps.document", "modifiedTime": "2026-09-01T00:00:00Z"})))
        .mount(&server)
        .await;
    Mock::given(path("/drive/v3/files/F1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "F1", "name": "Meet Recordings"})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/drive/v3/files/NOTES1/export"))
        .and(query_param("mimeType", "text/markdown"))
        .respond_with(ResponseTemplate::new(200).set_body_string(NOTES))
        .mount(&server)
        .await;
    Mock::given(path("/drive/v3/files/AGENDA1/export"))
        .respond_with(ResponseTemplate::new(200).set_body_string("# Agenda\n\n- item\n"))
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
        let client =
            OAuthClient::from_json(r#"{"installed":{"client_id":"c","client_secret":"s"}}"#)
                .unwrap()
                .with_endpoints(format!("{}/auth", self.0), format!("{}/token", self.0));
        let tokens = Arc::new(TokenProvider::new(client, Secret::new("rt")));
        let api = GoogleApi::new(tokens)?.with_base(&self.0);
        Ok(Some(Arc::new(MeetSource::new(account.ctx(), Some(api))?)))
    }
}

fn pipeline(home: &Home, server: &MockServer) -> Pipeline {
    let clock = Arc::new(FixedClock::new(
        second_brain_kernel::util::parse_ts("2026-09-10T00:00:00Z").unwrap(),
    ));
    Pipeline::new(
        Catalog::open_with_clock(home, clock).unwrap(),
        Arc::new(Factory(server.uri())),
    )
}

async fn export_calls(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/drive/v3/files/NOTES1/export")
        .count()
}

#[tokio::test]
async fn calendar_strategy_native_summary_and_skip_unchanged() {
    let modified = Arc::new(Mutex::new("2026-09-09T02:00:00Z".to_string()));
    let server = google_mock(modified.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    Catalog::create(&home)
        .unwrap()
        .add_account(
            &AccountId::new("work").unwrap(),
            AccountKind::Google,
            "Work",
            Some("alice@example.test"),
            &sb_google::default_config_json(&["meet".into()]),
        )
        .unwrap();
    let p = pipeline(&home, &server);
    let r = p.sync(&SyncOptions::default()).await.unwrap();
    assert!(r.stats.errors.is_empty(), "{:?}", r.stats.errors);
    let s = &r.stats.sources["work/google.meet"];
    assert_eq!((s.new, s.not_applicable), (1, 1), "{s:?}");
    {
        let cat = p.catalog();
        assert!(
            cat.entry_by_key("work", SourceKind::GoogleMeet, "AGENDA1")
                .unwrap()
                .is_none()
        );
        assert!(
            cat.entry_by_key("work", SourceKind::GoogleMeet, "NOTES2")
                .unwrap()
                .is_none(),
            "declined"
        );
        let e = cat
            .entry_by_key("work", SourceKind::GoogleMeet, "NOTES1")
            .unwrap()
            .unwrap();
        assert_eq!(e.title, "Design Review");
        assert_eq!(e.raw_status, RawStatus::Present);
        assert_eq!(e.summary_status, SummaryStatus::Done);
        assert_eq!(
            e.source_created_at,
            second_brain_kernel::util::parse_ts("2026-09-09T01:00:00Z")
        );
        assert_eq!(e.metadata["recurring"], true);
        assert_eq!(e.metadata["has_transcript"], true);
        assert_eq!(e.metadata["absentees"][0]["name"], "Bob Example");
        let sum = cat.summary(e.id).unwrap().unwrap();
        assert_eq!(sum.generator_kind, GeneratorKind::SourceNative);
        assert_eq!(sum.model, "gemini-meet-notes");
        assert_eq!(sum.prompt_version, None);
        let secs = cat.sections(e.id).unwrap();
        assert_eq!(secs.len(), 4);
        let details = secs
            .iter()
            .find(|s| s.kind == SectionKind::Details)
            .unwrap();
        assert_eq!(details.text, "* 詳細な議論 (00:01:02)");
    }
    assert_eq!(export_calls(&server).await, 1);

    // Unchanged modifiedTime: no export.
    let r = p.sync(&SyncOptions::default()).await.unwrap();
    assert_eq!(r.stats.sources["work/google.meet"].unchanged, 1);
    assert_eq!(export_calls(&server).await, 1);

    // Changed modifiedTime: replaced (segment 0 only).
    *modified.lock().unwrap() = "2026-09-09T05:00:00Z".to_string();
    let r = p.sync(&SyncOptions::default()).await.unwrap();
    assert_eq!(r.stats.sources["work/google.meet"].updated, 1);
    assert_eq!(export_calls(&server).await, 2);
    let cat = p.catalog();
    let e = cat
        .entry_by_key("work", SourceKind::GoogleMeet, "NOTES1")
        .unwrap()
        .unwrap();
    let raws = cat.raw_objects(e.id).unwrap();
    assert_eq!(raws.len(), 1);
    assert_eq!(raws[0].seq, 0);
}

// ---------- windows and coverage (ADR-0016) ----------

fn pipeline_at(home: &Home, server: &MockServer, now: &str) -> Pipeline {
    let clock = Arc::new(FixedClock::new(
        second_brain_kernel::util::parse_ts(now).unwrap(),
    ));
    Pipeline::new(
        Catalog::open_with_clock(home, clock).unwrap(),
        Arc::new(Factory(server.uri())),
    )
}

fn param(r: &Request, key: &str) -> Option<String> {
    r.url
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.to_string())
}

/// `(timeMin, timeMax)` of every Calendar request, in order.
async fn calendar_windows(server: &MockServer) -> Vec<(String, String)> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/calendar/v3/calendars/primary/events")
        .map(|r| (param(r, "timeMin").unwrap(), param(r, "timeMax").unwrap()))
        .collect()
}

/// The `q` of every Drive list request that targets the notes folder.
async fn drive_queries(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/drive/v3/files")
        .filter_map(|r| param(r, "q"))
        .filter(|q| q.contains("in parents"))
        .collect()
}

fn cursor(p: &Pipeline, key: &str) -> serde_json::Value {
    p.catalog()
        .cursor(
            &AccountId::new("work").unwrap(),
            SourceKind::GoogleMeet,
            key,
        )
        .unwrap()
        .unwrap_or_default()
}

fn window_home() -> (tempfile::TempDir, Home) {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let mut config = sb_google::default_config_json(&["meet".into()]);
    config["meet_folder_id"] = json!("F1");
    Catalog::create(&home)
        .unwrap()
        .add_account(
            &AccountId::new("work").unwrap(),
            AccountKind::Google,
            "Work",
            Some("alice@example.test"),
            &config,
        )
        .unwrap();
    (dir, home)
}

fn no_summary(since: Option<&str>, until: Option<&str>) -> SyncOptions {
    SyncOptions {
        no_summary: true,
        since: since.map(|s| second_brain_kernel::util::parse_ts(s).unwrap()),
        until: until.map(|s| second_brain_kernel::util::parse_ts(s).unwrap()),
        ..Default::default()
    }
}

#[tokio::test]
async fn first_window_is_thirty_days_and_later_runs_follow_the_cursors() {
    let server = google_mock(Arc::new(Mutex::new("2026-09-09T02:00:00Z".into()))).await;
    let (_dir, home) = window_home();

    let p = pipeline_at(&home, &server, "2026-09-10T00:00:00Z");
    let r = p.sync(&no_summary(None, None)).await.unwrap();
    assert!(r.stats.errors.is_empty(), "{:?}", r.stats.errors);
    // Calendar: 30 days back, one hour ahead. Drive: modified in the last 30 days.
    assert_eq!(
        calendar_windows(&server).await,
        vec![("2026-08-11T00:00:00Z".into(), "2026-09-10T01:00:00Z".into())]
    );
    assert!(drive_queries(&server).await[0].ends_with("modifiedTime > '2026-08-11T00:00:00Z'"));
    // Cursors stay five minutes behind the run start and record the coverage.
    assert_eq!(
        cursor(&p, "calendar")["last_time_max"],
        "2026-09-09T23:55:00Z"
    );
    assert_eq!(
        cursor(&p, "calendar")["covered_since"],
        "2026-08-11T00:00:00Z"
    );
    assert_eq!(
        cursor(&p, "drive")["modified_after"],
        "2026-09-09T23:55:00Z"
    );
    assert_eq!(cursor(&p, "drive")["covered_since"], "2026-08-11T00:00:00Z");

    // The next day: Calendar reaches three days behind its cursor (notes are
    // attached after an event ends); Drive continues from its cursor.
    let p = pipeline_at(&home, &server, "2026-09-11T00:00:00Z");
    p.sync(&no_summary(None, None)).await.unwrap();
    assert_eq!(
        calendar_windows(&server).await[1],
        ("2026-09-06T23:55:00Z".into(), "2026-09-11T01:00:00Z".into())
    );
    assert!(drive_queries(&server).await[1].ends_with("modifiedTime > '2026-09-09T23:55:00Z'"));
    assert_eq!(
        cursor(&p, "calendar")["covered_since"],
        "2026-08-11T00:00:00Z"
    );
}

#[tokio::test]
async fn since_fetches_only_what_is_older_than_the_coverage() {
    let server = google_mock(Arc::new(Mutex::new("2026-09-09T02:00:00Z".into()))).await;
    let (_dir, home) = window_home();
    let p = pipeline_at(&home, &server, "2026-09-10T00:00:00Z");
    p.sync(&no_summary(None, None)).await.unwrap();
    let p = pipeline_at(&home, &server, "2026-09-11T00:00:00Z");

    p.sync(&no_summary(Some("2026-07-01T00:00:00Z"), None))
        .await
        .unwrap();
    let windows = calendar_windows(&server).await;
    assert_eq!(
        windows.last().unwrap(),
        &("2026-07-01T00:00:00Z".into(), "2026-08-11T00:00:00Z".into())
    );
    assert!(drive_queries(&server).await.last().unwrap().ends_with(
        "modifiedTime > '2026-07-01T00:00:00Z' and modifiedTime <= '2026-08-11T00:00:00Z'"
    ));
    for key in ["calendar", "drive"] {
        assert_eq!(
            cursor(&p, key)["covered_since"],
            "2026-07-01T00:00:00Z",
            "{key}"
        );
    }
    // The forward cursor of the Drive strategy did not move backwards.
    assert_eq!(
        cursor(&p, "drive")["modified_after"],
        "2026-09-10T23:55:00Z"
    );

    // Already covered: nothing more is requested backwards.
    let calls = calendar_windows(&server).await.len();
    p.sync(&no_summary(Some("2026-07-15T00:00:00Z"), None))
        .await
        .unwrap();
    assert_eq!(
        calendar_windows(&server).await.len(),
        calls + 1,
        "forward only"
    );
}

#[tokio::test]
async fn detached_window_is_fetched_but_not_recorded() {
    let server = google_mock(Arc::new(Mutex::new("2026-09-09T02:00:00Z".into()))).await;
    let (_dir, home) = window_home();
    let p = pipeline_at(&home, &server, "2026-09-10T00:00:00Z");
    p.sync(&no_summary(None, None)).await.unwrap();
    p.sync(&no_summary(
        Some("2026-05-01T00:00:00Z"),
        Some("2026-06-01T00:00:00Z"),
    ))
    .await
    .unwrap();
    assert_eq!(
        calendar_windows(&server).await.last().unwrap(),
        &("2026-05-01T00:00:00Z".into(), "2026-06-01T00:00:00Z".into())
    );
    assert_eq!(
        cursor(&p, "calendar")["covered_since"],
        "2026-08-11T00:00:00Z"
    );
    assert_eq!(cursor(&p, "drive")["covered_since"], "2026-08-11T00:00:00Z");
}
