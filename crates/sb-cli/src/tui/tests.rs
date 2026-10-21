//! Screen tests: keys go through `App::on_key`, the catalog is a temporary
//! home, and drawing uses ratatui's test backend.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use second_brain_kernel::{AccountId, AccountKind, RunTrigger};
use second_brain_store::Home;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::Start;
use super::app::{App, Job, Modal, Outcome, Section, edit_text, parse_edit};
use super::view;
use crate::Ctx;

fn ctx() -> (tempfile::TempDir, Ctx) {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::resolve(Some(&dir.path().join("home"))).unwrap();
    second_brain_setup::home::setup_home(&home).unwrap();
    let ctx = Ctx {
        home,
        json: false,
        quiet: true,
        trigger: RunTrigger::Manual,
        cancel: CancellationToken::new(),
    };
    (dir, ctx)
}

fn press(app: &mut App, ctx: &Ctx, code: KeyCode) -> Outcome {
    app.on_key(ctx, KeyEvent::new(code, KeyModifiers::NONE))
}

fn typed(app: &mut App, ctx: &Ctx, s: &str) {
    for c in s.chars() {
        press(app, ctx, KeyCode::Char(c));
    }
}

/// Clear the focused text field (Ctrl+U).
fn clear(app: &mut App, ctx: &Ctx) {
    app.on_key(
        ctx,
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
    );
}

/// Move the cursor to the row whose text contains `needle`.
fn select(app: &mut App, ctx: &Ctx, needle: &str) {
    press(app, ctx, KeyCode::Home);
    let rows = app.rows(app.current());
    let i = rows
        .iter()
        .position(|r| r.text.contains(needle))
        .unwrap_or_else(|| panic!("no row with {needle:?}"));
    for _ in 0..i {
        press(app, ctx, KeyCode::Down);
    }
    assert!(app.rows(app.current())[app.cursor()].text.contains(needle));
}

fn screen(app: &App) -> String {
    let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
    t.draw(|f| view::render(app, f)).unwrap();
    let buf = t.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn add_slack(ctx: &Ctx) {
    ctx.catalog()
        .unwrap()
        .add_account(
            &AccountId::new("acme-slack").unwrap(),
            AccountKind::Slack,
            "Acme",
            Some("T1:U1"),
            &second_brain_slack::default_config_json(),
        )
        .unwrap();
}

#[test]
fn edit_text_round_trips_by_shape() {
    assert_eq!(edit_text(&json!(["a", "b"])), "a, b");
    assert_eq!(parse_edit("a, b,", Some(&json!([]))), json!(["a", "b"]));
    assert_eq!(edit_text(&json!(["{a,b}"])), r#"["{a,b}"]"#);
    assert_eq!(
        parse_edit(r#"["{a,b}"]"#, Some(&json!([]))),
        json!(["{a,b}"])
    );
    assert_eq!(parse_edit("123", Some(&json!("x"))), json!("123"));
    assert_eq!(parse_edit("45", Some(&json!(30))), json!(45));
    assert_eq!(parse_edit("true", None), json!(true));
}

#[test]
fn navigation_moves_between_sections() {
    let (_d, ctx) = ctx();
    let mut app = App::new(&ctx, Start::Setup).unwrap();
    assert_eq!(app.current(), Section::Home);
    press(&mut app, &ctx, KeyCode::Tab);
    assert_eq!(app.current(), Section::Accounts);
    press(&mut app, &ctx, KeyCode::BackTab);
    press(&mut app, &ctx, KeyCode::BackTab);
    assert_eq!(app.current(), Section::Secrets);
    press(&mut app, &ctx, KeyCode::Char('3'));
    assert_eq!(app.current(), Section::Summarizer);
    assert!(matches!(
        press(&mut app, &ctx, KeyCode::Char('q')),
        Outcome::Quit
    ));
    let app = App::new(&ctx, Start::Settings).unwrap();
    assert_eq!(app.current(), Section::Settings);
}

#[test]
fn settings_are_edited_validated_and_reset() {
    let (_d, ctx) = ctx();
    let mut app = App::new(&ctx, Start::Settings).unwrap();
    select(&mut app, &ctx, "summary.min_chars");
    press(&mut app, &ctx, KeyCode::Enter);
    assert!(matches!(app.modals.last(), Some(Modal::Form(..))));
    clear(&mut app, &ctx);
    typed(&mut app, &ctx, "abc");
    press(&mut app, &ctx, KeyCode::Enter);
    // Invalid: the form stays open with an error.
    assert!(app.status.as_ref().is_some_and(|s| s.error));
    assert!(matches!(app.modals.last(), Some(Modal::Form(..))));
    clear(&mut app, &ctx);
    typed(&mut app, &ctx, "200");
    press(&mut app, &ctx, KeyCode::Enter);
    assert!(app.modals.is_empty());
    let cat = ctx.catalog().unwrap();
    assert_eq!(
        cat.setting_raw("summary.min_chars").unwrap(),
        Some(json!(200))
    );

    // Booleans toggle in place.
    select(&mut app, &ctx, "ingest.keep_original");
    press(&mut app, &ctx, KeyCode::Enter);
    assert_eq!(
        cat.setting_raw("ingest.keep_original").unwrap(),
        Some(json!(true))
    );

    // Choices open a list.
    select(&mut app, &ctx, "summary.language");
    press(&mut app, &ctx, KeyCode::Enter);
    assert!(matches!(app.modals.last(), Some(Modal::Pick { .. })));
    press(&mut app, &ctx, KeyCode::Down);
    press(&mut app, &ctx, KeyCode::Enter);
    assert_eq!(
        cat.setting_raw("summary.language").unwrap(),
        Some(json!("ja"))
    );

    // Lists are comma-separated.
    select(&mut app, &ctx, "ingest.local.deny");
    press(&mut app, &ctx, KeyCode::Enter);
    typed(&mut app, &ctx, "**/*.pem, **/.env");
    press(&mut app, &ctx, KeyCode::Enter);
    assert_eq!(
        cat.setting_raw("ingest.local.deny").unwrap(),
        Some(json!(["**/*.pem", "**/.env"]))
    );

    // x resets to the default.
    select(&mut app, &ctx, "summary.min_chars");
    press(&mut app, &ctx, KeyCode::Char('x'));
    assert_eq!(cat.setting_raw("summary.min_chars").unwrap(), None);
}

#[test]
fn read_only_settings_are_not_edited() {
    let (_d, ctx) = ctx();
    let cat = ctx.catalog().unwrap();
    cat.set_setting("skills.installed", &json!(["claude"]))
        .unwrap();
    let mut app = App::new(&ctx, Start::Settings).unwrap();
    select(&mut app, &ctx, "skills.installed");
    press(&mut app, &ctx, KeyCode::Enter);
    assert!(app.modals.is_empty());
    press(&mut app, &ctx, KeyCode::Char('x'));
    assert_eq!(
        cat.setting_raw("skills.installed").unwrap(),
        Some(json!(["claude"]))
    );
}

#[test]
fn account_config_is_edited_and_validated() {
    let (_d, ctx) = ctx();
    add_slack(&ctx);
    let mut app = App::new(&ctx, Start::Account("acme-slack".into())).unwrap();
    assert_eq!(app.current(), Section::Accounts);
    let Some(Modal::Account(ed)) = app.modals.last() else {
        panic!("no account editor");
    };
    let at = |key: &str, app: &App| match app.modals.last() {
        Some(Modal::Account(ed)) => ed.rows.iter().position(|r| r.key == key).unwrap(),
        _ => panic!("no account editor"),
    };
    assert!(ed.rows.iter().any(|r| r.key == "full_channels"));
    let go = |key: &str, app: &mut App| {
        let i = at(key, app);
        if let Some(Modal::Account(ed)) = app.modals.last_mut() {
            ed.cursor = i;
        }
    };
    go("full_channels", &mut app);
    press(&mut app, &ctx, KeyCode::Enter);
    typed(&mut app, &ctx, "general, dev");
    press(&mut app, &ctx, KeyCode::Enter);
    let config = |ctx: &Ctx| {
        ctx.catalog()
            .unwrap()
            .account_by_str("acme-slack")
            .unwrap()
            .unwrap()
            .config
    };
    assert_eq!(config(&ctx)["full_channels"], json!(["general", "dev"]));

    go("include_dms", &mut app);
    press(&mut app, &ctx, KeyCode::Enter);
    assert_eq!(config(&ctx)["include_dms"], json!(false));

    // A wrong type is refused and nothing is written.
    go("fetch_jobs", &mut app);
    press(&mut app, &ctx, KeyCode::Enter);
    clear(&mut app, &ctx);
    typed(&mut app, &ctx, "many");
    press(&mut app, &ctx, KeyCode::Enter);
    assert!(app.status.as_ref().is_some_and(|s| s.error));
    press(&mut app, &ctx, KeyCode::Esc);
    assert_eq!(config(&ctx)["fetch_jobs"], json!(4));

    go("mention_scan", &mut app);
    press(&mut app, &ctx, KeyCode::Enter);
    press(&mut app, &ctx, KeyCode::Down);
    press(&mut app, &ctx, KeyCode::Enter);
    assert_eq!(config(&ctx)["mention_scan"], json!("search_only"));

    // x removes a stored key so the default applies.
    go("mention_scan", &mut app);
    press(&mut app, &ctx, KeyCode::Char('x'));
    assert_eq!(config(&ctx).get("mention_scan"), None);

    press(&mut app, &ctx, KeyCode::Esc);
    assert!(app.modals.is_empty());
    // Disabling from the list.
    select(&mut app, &ctx, "acme-slack");
    press(&mut app, &ctx, KeyCode::Char('x'));
    let a = ctx
        .catalog()
        .unwrap()
        .account_by_str("acme-slack")
        .unwrap()
        .unwrap();
    assert_eq!(a.status.to_string(), "disabled");
}

#[test]
fn adding_accounts_returns_jobs() {
    let (_d, ctx) = ctx();
    let mut app = App::new(&ctx, Start::Setup).unwrap();
    app.goto(Section::Accounts);
    select(&mut app, &ctx, "Add a Slack workspace");
    press(&mut app, &ctx, KeyCode::Enter);
    // Required fields are checked before the job runs.
    press(&mut app, &ctx, KeyCode::Tab);
    press(&mut app, &ctx, KeyCode::Tab);
    press(&mut app, &ctx, KeyCode::Tab);
    assert!(matches!(
        press(&mut app, &ctx, KeyCode::Enter),
        Outcome::None
    ));
    assert!(app.status.as_ref().is_some_and(|s| s.error));
    press(&mut app, &ctx, KeyCode::Tab);
    typed(&mut app, &ctx, "acme");
    press(&mut app, &ctx, KeyCode::Enter);
    typed(&mut app, &ctx, "xoxp-1");
    // The token is masked on screen.
    assert!(!screen(&app).contains("xoxp-1"));
    press(&mut app, &ctx, KeyCode::Enter);
    press(&mut app, &ctx, KeyCode::Enter);
    match press(&mut app, &ctx, KeyCode::Enter) {
        Outcome::Job(Job::AddSlack { id, token, label }) => {
            assert_eq!(id, "acme");
            assert_eq!(token.expose(), "xoxp-1");
            assert_eq!(label, None);
        }
        _ => panic!("expected the Slack job"),
    }
    assert!(app.modals.is_empty());
}

#[test]
fn summarizer_form_stores_the_key_and_returns_the_job() {
    let (_d, ctx) = ctx();
    let mut app = App::new(&ctx, Start::Setup).unwrap();
    app.goto(Section::Summarizer);
    select(&mut app, &ctx, "Add a summarizer");
    press(&mut app, &ctx, KeyCode::Enter);
    // The first preset is the Anthropic API.
    press(&mut app, &ctx, KeyCode::Enter);
    let Some(Modal::Form(form, _)) = app.modals.last() else {
        panic!("no form");
    };
    assert!(form.fields.iter().any(|f| f.key == "api_key"));
    press(&mut app, &ctx, KeyCode::Down);
    press(&mut app, &ctx, KeyCode::Down);
    typed(&mut app, &ctx, "sk-test");
    press(&mut app, &ctx, KeyCode::Down);
    press(&mut app, &ctx, KeyCode::Down);
    // Turn the test call off.
    press(&mut app, &ctx, KeyCode::Char(' '));
    press(&mut app, &ctx, KeyCode::Down);
    match press(&mut app, &ctx, KeyCode::Enter) {
        Outcome::Job(Job::Llm {
            name,
            profile,
            make_default,
            test,
            ..
        }) => {
            assert_eq!(name, "anthropic");
            assert_eq!(profile["provider"], json!("anthropic"));
            assert!(make_default);
            assert!(!test);
        }
        _ => panic!("expected the summarizer job"),
    }
    let cat = ctx.catalog().unwrap();
    let key = cat
        .secret(
            &second_brain_store::SecretScope::Global,
            "anthropic.api_key",
        )
        .unwrap()
        .unwrap();
    assert_eq!(key.expose(), "sk-test");
}

#[test]
fn schedule_fields_are_checked() {
    let (_d, ctx) = ctx();
    let mut app = App::new(&ctx, Start::Setup).unwrap();
    app.goto(Section::Schedule);
    select(&mut app, &ctx, "Daily sync time");
    press(&mut app, &ctx, KeyCode::Enter);
    clear(&mut app, &ctx);
    typed(&mut app, &ctx, "25:00");
    press(&mut app, &ctx, KeyCode::Enter);
    assert!(app.status.as_ref().is_some_and(|s| s.error));
    clear(&mut app, &ctx);
    typed(&mut app, &ctx, "07:15");
    press(&mut app, &ctx, KeyCode::Enter);
    assert!(app.modals.is_empty());
    assert_eq!(app.schedule.time, "07:15");
    assert!(screen(&app).contains("Daily sync time        07:15"));
}

#[test]
fn first_sync_days_are_validated() {
    let (_d, ctx) = ctx();
    let mut app = App::new(&ctx, Start::Setup).unwrap();
    app.goto(Section::FirstSync);
    press(&mut app, &ctx, KeyCode::Enter);
    clear(&mut app, &ctx);
    typed(&mut app, &ctx, "0");
    press(&mut app, &ctx, KeyCode::Enter);
    assert!(app.status.as_ref().is_some_and(|s| s.error));
    clear(&mut app, &ctx);
    typed(&mut app, &ctx, "60");
    press(&mut app, &ctx, KeyCode::Enter);
    let cat = ctx.catalog().unwrap();
    assert_eq!(
        cat.setting_raw("sync.initial_days").unwrap(),
        Some(json!(60))
    );
    press(&mut app, &ctx, KeyCode::Down);
    assert!(matches!(
        press(&mut app, &ctx, KeyCode::Enter),
        Outcome::Job(Job::Estimate)
    ));
}

#[test]
fn secrets_are_stored_masked_and_removed_after_confirmation() {
    let (_d, ctx) = ctx();
    let mut app = App::new(&ctx, Start::Settings).unwrap();
    app.goto(Section::Secrets);
    select(&mut app, &ctx, "Store a secret");
    press(&mut app, &ctx, KeyCode::Enter);
    typed(&mut app, &ctx, "openai.api_key");
    press(&mut app, &ctx, KeyCode::Enter);
    press(&mut app, &ctx, KeyCode::Enter);
    typed(&mut app, &ctx, "sk-secret-value");
    press(&mut app, &ctx, KeyCode::Enter);
    press(&mut app, &ctx, KeyCode::Enter);
    assert!(app.modals.is_empty(), "{:?}", app.status);
    let shown = screen(&app);
    assert!(shown.contains("openai.api_key"));
    assert!(!shown.contains("sk-secret-value"));
    select(&mut app, &ctx, "openai.api_key");
    press(&mut app, &ctx, KeyCode::Char('x'));
    assert!(matches!(app.modals.last(), Some(Modal::Confirm { .. })));
    press(&mut app, &ctx, KeyCode::Char('n'));
    let cat = ctx.catalog().unwrap();
    let global = second_brain_store::SecretScope::Global;
    assert!(cat.secret(&global, "openai.api_key").unwrap().is_some());
    press(&mut app, &ctx, KeyCode::Char('x'));
    press(&mut app, &ctx, KeyCode::Char('y'));
    assert!(cat.secret(&global, "openai.api_key").unwrap().is_none());
}

#[test]
fn profiles_are_made_default_and_removed() {
    let (_d, ctx) = ctx();
    let cat = ctx.catalog().unwrap();
    let p: Value = json!({"kind": "llm_cli", "provider": "claude_cli", "model": "haiku"});
    cat.set_setting("llm.profiles.a", &p).unwrap();
    cat.set_setting("llm.profiles.b", &p).unwrap();
    cat.set_setting("summary.profile.default", &json!("a"))
        .unwrap();
    let mut app = App::new(&ctx, Start::Setup).unwrap();
    app.goto(Section::Summarizer);
    select(&mut app, &ctx, " b ");
    press(&mut app, &ctx, KeyCode::Enter);
    assert_eq!(
        cat.setting_raw("summary.profile.default").unwrap(),
        Some(json!("b"))
    );
    press(&mut app, &ctx, KeyCode::Char('x'));
    press(&mut app, &ctx, KeyCode::Char('y'));
    assert_eq!(cat.setting_raw("llm.profiles.b").unwrap(), None);
    assert_eq!(cat.setting_raw("summary.profile.default").unwrap(), None);
}

#[test]
fn screen_draws_sections_and_help() {
    let (_d, ctx) = ctx();
    add_slack(&ctx);
    let mut app = App::new(&ctx, Start::Setup).unwrap();
    let s = screen(&app);
    assert!(s.contains("second-brain setup"));
    assert!(s.contains("1 Home"));
    assert!(s.contains("8 Secrets"));
    assert!(s.contains("q quit"));
    press(&mut app, &ctx, KeyCode::Tab);
    let s = screen(&app);
    assert!(s.contains("acme-slack"));
    assert!(s.contains("+ Add a Slack workspace"));
    assert!(s.contains("Enter edit config · x disable"));
    // A small terminal still draws.
    let mut t = Terminal::new(TestBackend::new(30, 8)).unwrap();
    t.draw(|f| view::render(&app, f)).unwrap();
}
