//! End-to-end tests of the `sb` binary (no network access).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output};

use second_brain_kernel::{AccountId, AccountKind};
use serde_json::{Value, json};

fn sb(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sb"))
        .arg("--home")
        .arg(home)
        .args(args)
        .env_remove("SB_TRIGGER")
        .env("SB_LOG", "error")
        .output()
        .unwrap()
}

fn json_of(o: &Output) -> Value {
    let s = String::from_utf8_lossy(&o.stdout);
    serde_json::from_str(s.trim()).unwrap_or_else(|e| panic!("not JSON ({e}): {s}"))
}

fn bundle(dir: &Path) {
    std::fs::create_dir_all(dir.join("raw")).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        json!({"format": "second-brain-import/v1", "created_at": "2026-10-01T12:00:00Z", "producer": "test/1",
               "accounts": {"google": "work"}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("raw/doc1.md"),
        "## Daily Dev Standup\n\n### **概要**\n\n定例\n",
    )
    .unwrap();
    let lines = [
        json!({"source_kind": "google.meet", "source_id": "DOC1", "source_url": "https://docs.google.com/document/d/DOC1/edit",
               "title": "Daily Dev Standup", "source_created_at": "2026-09-03T00:15:00Z",
               "sections": [{"kind": "overview", "text": "定例の打ち合わせ。契約の件。"},
                            {"kind": "decisions", "text": "- CSV形式で進めることに決定した"}],
               "summary": {"generator_kind": "source_native", "provider": "google", "model": "gemini-meet-notes"},
               "raw": [{"role": "notes", "path": "raw/doc1.md"}]}),
        json!({"source_kind": "google.meet", "source_id": "DOC2", "title": "Planning",
               "sections": [{"kind": "details", "origin": "extracted", "text": "Planning notes about the roadmap"}]}),
    ];
    std::fs::write(
        dir.join("entries.jsonl"),
        lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
    )
    .unwrap();
}

#[test]
fn uninitialized_home_and_usage_errors() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("h");
    let o = sb(&home, &["search", "x", "--json"]);
    assert_eq!(o.status.code(), Some(2));
    let v = json_of(&o);
    assert_eq!(v["schema"], "sb.error/v1");
    assert_eq!(v["error"]["code"], "home.not_initialized");
    let o = sb(&home, &["search", "--json"]);
    assert_eq!(o.status.code(), Some(64));
    assert_eq!(json_of(&o)["error"]["code"], "usage");
    let o = sb(&home, &["--help"]);
    assert_eq!(o.status.code(), Some(0));
}

#[test]
fn sync_range_options_are_validated() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("h");
    assert_eq!(
        sb(&home, &["setup", "home", "--yes"]).status.code(),
        Some(0)
    );
    for args in [
        &["sync", "--until", "2026-07-01"][..],
        &["sync", "--since", "2026-08-01", "--until", "2026-07-01"],
        &["sync", "--since", "soon"],
    ] {
        let o = sb(&home, &[args, &["--json"]].concat());
        assert_eq!(o.status.code(), Some(64), "{args:?}");
        assert_eq!(json_of(&o)["error"]["code"], "usage", "{args:?}");
    }
    for bad in ["0", "-3", "\"soon\"", "1.5"] {
        let o = sb(
            &home,
            &["config", "set", "sync.initial_days", bad, "--json"],
        );
        assert_eq!(o.status.code(), Some(64), "initial_days {bad}");
    }
    assert_eq!(
        sb(&home, &["config", "set", "sync.initial_days", "45"])
            .status
            .code(),
        Some(0)
    );
    // Valid ranges are accepted (nothing to sync without accounts).
    let o = sb(&home, &["sync", "--since", "90d", "--no-summary", "--json"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(json_of(&o)["coverage"].as_array().unwrap().is_empty());
}

#[test]
fn import_search_show_list_stats_doctor() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("h");
    assert!(sb(&home, &["setup", "home"]).status.success());
    {
        let cat = sb_store::Catalog::open(&sb_store::Home::new(&home)).unwrap();
        cat.add_account(
            &AccountId::new("work").unwrap(),
            AccountKind::Google,
            "Work",
            Some("a@example.test"),
            &json!({"features": ["meet"]}),
        )
        .unwrap();
    }
    let b = d.path().join("bundle");
    bundle(&b);
    let o = sb(
        &home,
        &["import", b.to_str().unwrap(), "--dry-run", "--json"],
    );
    assert!(o.status.success());
    assert_eq!(json_of(&o)["created"], 0);
    let o = sb(&home, &["import", b.to_str().unwrap(), "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v = json_of(&o);
    assert_eq!(
        (v["created"].as_u64(), v["raw_missing"].as_u64()),
        (Some(2), Some(1))
    );

    // Two-character Japanese term (LIKE path) and a trigram term.
    let v = json_of(&sb(&home, &["search", "契約", "--json"]));
    assert_eq!(v["schema"], "sb.search/v1");
    assert_eq!(v["hits"].as_array().unwrap().len(), 1);
    let hit = &v["hits"][0];
    assert_eq!(hit["account"], json!({"id": "work", "label": "Work"}));
    assert_eq!(
        hit["cite_url"],
        "https://docs.google.com/document/d/DOC1/edit"
    );
    let v = json_of(&sb(&home, &["search", "roadmap", "--json"]));
    assert!(
        v["hits"][0]["cite_url"].is_null(),
        "no link and no raw file"
    );
    let v = json_of(&sb(
        &home,
        &["search", "CSV", "--section", "decisions", "--json"],
    ));
    let uid = v["hits"][0]["entry_uid"].as_str().unwrap().to_string();
    assert_eq!(v["hits"][0]["section"], "decisions");

    let v = json_of(&sb(
        &home,
        &["show", &uid, "--section", "decisions", "--json"],
    ));
    assert_eq!(v["entry"]["sections"].as_array().unwrap().len(), 1);
    assert_eq!(v["entry"]["summary"]["model"], "gemini-meet-notes");
    let o = sb(&home, &["show", &uid, "--raw", "--role", "notes"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("### **概要**"));
    let o = sb(&home, &["show", "NOPE", "--json"]);
    assert_eq!(o.status.code(), Some(2));

    let v = json_of(&sb(&home, &["list", "--json", "--raw-status", "missing"]));
    assert_eq!(v["total"], 1);
    let v = json_of(&sb(&home, &["stats", "--json"]));
    assert_eq!(v["entries"], 2);
    let v = json_of(&sb(&home, &["review", "--json"]));
    assert_eq!(v["entries"].as_array().unwrap().len(), 1);

    let o = sb(&home, &["doctor", "--json"]);
    let v = json_of(&o);
    assert_eq!(v["schema"], "sb.doctor/v1");
    let failing: Vec<&Value> = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["status"] == "error")
        .collect();
    assert_eq!(v["status"], "warning", "failing checks: {failing:#?}");
    assert_eq!(o.status.code(), Some(1));
    let ids: Vec<&str> = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    for id in [
        "home.exists",
        "home.permissions",
        "db.migrations",
        "raw.consistency",
        "index.consistency",
        "disk.usage",
    ] {
        assert!(ids.contains(&id), "{id} missing in {ids:?}");
    }

    // A deleted raw file is detected and fixed by marking the entry missing.
    let rows = {
        let cat = sb_store::Catalog::open(&sb_store::Home::new(&home)).unwrap();
        let e = cat.entry_by_uid(&uid).unwrap().unwrap();
        cat.raw_objects(e.id).unwrap()
    };
    std::fs::remove_file(sb_store::Home::new(&home).resolve_rel(&rows[0].path)).unwrap();
    let check = |v: &Value, id: &str| {
        v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap()
            .clone()
    };
    let v = json_of(&sb(&home, &["doctor", "--json"]));
    assert_eq!(check(&v, "raw.consistency")["status"], "warning");
    let v = json_of(&sb(&home, &["doctor", "--fix", "--json"]));
    assert_eq!(check(&v, "raw.consistency")["fixed"], true);
    let v = json_of(&sb(&home, &["list", "--json", "--raw-status", "missing"]));
    assert_eq!(v["total"], 2);

    let v = json_of(&sb(&home, &["index", "rebuild", "--json"]));
    assert_eq!(v["rows"], 3);
}

#[test]
fn config_and_schedule_dry_run() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("h");
    sb(&home, &["setup", "home"]);
    assert!(
        sb(&home, &["config", "set", "summary.min_chars", "200"])
            .status
            .success()
    );
    let v = json_of(&sb(
        &home,
        &["config", "get", "summary.min_chars", "--json"],
    ));
    assert_eq!(v["value"], 200);
    assert_eq!(
        sb(&home, &["config", "set", "summary.min_chars", "\"x\""])
            .status
            .code(),
        Some(64)
    );
    assert_eq!(
        sb(
            &home,
            &[
                "config",
                "set",
                "llm.profiles.bad",
                r#"{"kind":"llm_api","provider":"anthropic"}"#
            ]
        )
        .status
        .code(),
        Some(64)
    );
    assert!(
        sb(
            &home,
            &[
                "config",
                "set",
                "llm.profiles.fast",
                r#"{"kind":"llm_api","provider":"anthropic","model":"claude-haiku-4-5"}"#
            ]
        )
        .status
        .success()
    );
    let v = json_of(&sb(
        &home,
        &[
            "setup",
            "schedule",
            "--dry-run",
            "--time",
            "07:05",
            "--json",
        ],
    ));
    assert_eq!(v["dry_run"], true);
    assert!(!v["files"].as_array().unwrap().is_empty());
    assert_eq!(
        sb(
            &home,
            &["setup", "schedule", "--dry-run", "--time", "25:00"]
        )
        .status
        .code(),
        Some(64)
    );
    let v = json_of(&sb(&home, &["sync", "--dry-run", "--json"]));
    assert_eq!(v["dry_run"]["accounts"].as_array().unwrap().len(), 2);
}

#[cfg(unix)]
#[test]
fn skills_install_into_user_home() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("h");
    let user = d.path().join("user");
    std::fs::create_dir_all(&user).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_sb"))
            .arg("--home")
            .arg(&home)
            .args(args)
            .env("HOME", &user)
            .output()
            .unwrap()
    };
    run(&["setup", "home"]);
    let o = run(&["setup", "skills", "--target", "claude", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(user.join(".claude/skills/second-brain/SKILL.md").is_file());
    let v = json_of(&run(&["doctor", "--json"]));
    let s = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "skills.version")
        .unwrap()
        .clone();
    assert_eq!(s["status"], "ok");
    assert!(
        run(&["setup", "skills", "--target", "all", "--remove"])
            .status
            .success()
    );
    assert!(!user.join(".claude/skills/second-brain").exists());
}

#[test]
fn budget_settings_are_validated_and_stored() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("h");
    sb(&home, &["setup", "home"]);

    // The defaults are stored values, not just built-in defaults.
    let v = json_of(&sb(&home, &["config", "list", "--json"]));
    let item = |key: &str| {
        v["settings"]
            .as_array()
            .or_else(|| v.as_array())
            .unwrap_or_else(|| panic!("no list in {v}"))
            .iter()
            .find(|i| i["key"] == key)
            .unwrap_or_else(|| panic!("{key} missing in {v}"))
            .clone()
    };
    assert_eq!(item("summary.budget.weekly_usd")["value"], 2.0);
    assert_eq!(item("summary.budget.monthly_usd")["value"], 10.0);
    assert_eq!(item("summary.budget.weekly_usd")["default"], false);

    for ok in ["3.5", "0", "null", "10"] {
        let o = sb(&home, &["config", "set", "summary.budget.weekly_usd", ok]);
        assert!(
            o.status.success(),
            "{ok}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    for bad in ["-1", "\"two\"", "true", "[1]"] {
        let o = sb(&home, &["config", "set", "summary.budget.monthly_usd", bad]);
        assert_eq!(o.status.code(), Some(64), "{bad} must be rejected");
    }
    let v = json_of(&sb(
        &home,
        &["config", "get", "summary.budget.monthly_usd", "--json"],
    ));
    assert_eq!(v["value"], 10.0, "a rejected value changes nothing");

    assert!(
        sb(
            &home,
            &["config", "set", "summary.budget.timezone", "Asia/Tokyo"]
        )
        .status
        .success()
    );
    assert_eq!(
        sb(
            &home,
            &["config", "set", "summary.budget.timezone", "Nowhere/Land"]
        )
        .status
        .code(),
        Some(64)
    );
}

#[test]
fn budget_command_reports_history_and_feeds_stats_and_doctor() {
    use sb_store::{Catalog, Home, NewUsage, UsageOutcome};
    use second_brain_kernel::budget::{PeriodKind, Tz, period_containing};
    use second_brain_kernel::clock::FixedClock;
    use second_brain_kernel::{Generator, GeneratorKind, Usage};
    use std::sync::Arc;

    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("h");
    sb(&home, &["setup", "home"]);
    assert!(
        sb(&home, &["config", "set", "summary.budget.timezone", "UTC"])
            .status
            .success()
    );
    assert!(
        sb(&home, &["config", "set", "summary.budget.weekly_usd", "2"])
            .status
            .success()
    );

    // An empty ledger works, in both forms.
    let v = json_of(&sb(&home, &["budget", "--json"]));
    assert_eq!(v["schema"], "sb.budget/v1");
    assert_eq!(v["total"]["calls"], 0);
    assert_eq!(v["weeks"].as_array().unwrap().len(), 0);
    assert_eq!(v["by_model"].as_array().unwrap().len(), 0);
    let o = sb(&home, &["budget"]);
    assert!(o.status.success());
    assert!(
        String::from_utf8_lossy(&o.stdout).contains("Nothing has been spent yet"),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );

    let usage = |model: &str, usd: f64| NewUsage {
        run_id: None,
        entry_id: None,
        profile: "claude".into(),
        generator: Generator {
            kind: GeneratorKind::LlmApi,
            provider: "anthropic".into(),
            model: model.into(),
            prompt_version: None,
        },
        usage: Usage {
            input_tokens: 1_900_000,
            output_tokens: 200_000,
            calls: 4,
            ..Default::default()
        },
        cost_usd: Some(usd),
        outcome: UsageOutcome::Ok,
    };
    // Two weeks ago: the whole $2 cap was used and a run was stopped.
    let past = chrono::Utc::now() - chrono::Duration::days(14);
    let past_week = period_containing(PeriodKind::Week, past, Tz::UTC);
    {
        let cat =
            Catalog::open_with_clock(&Home::new(&home), Arc::new(FixedClock::new(past))).unwrap();
        cat.upsert_period(&past_week, Some(2.0)).unwrap();
        cat.record_usage(&usage("claude-haiku-4-5", 2.0)).unwrap();
        cat.mark_period_stopped(PeriodKind::Week, past_week.start)
            .unwrap();
    }
    // This week: $0.50 so far.
    {
        let cat = Catalog::open(&Home::new(&home)).unwrap();
        cat.record_usage(&usage("claude-haiku-4-5", 0.5)).unwrap();
    }

    let v = json_of(&sb(&home, &["budget", "--json", "--weeks", "3"]));
    assert_eq!(v["current"]["week"]["spent_usd"], 0.5);
    assert_eq!(v["current"]["week"]["cap_usd"], 2.0);
    let weeks = v["weeks"].as_array().unwrap();
    assert_eq!(weeks.len(), 1, "only evaluated weeks are listed");
    assert_eq!(weeks[0]["spent_usd"], 2.0);
    assert_eq!(weeks[0]["cap_usd"], 2.0);
    assert!(weeks[0]["stopped_at"].is_string());
    assert_eq!(
        weeks[0]["period_start"],
        past_week.start.format("%Y-%m-%d").to_string()
    );
    assert_eq!(v["total"]["spent_usd"], 2.5);
    assert_eq!(v["total"]["calls"], 8);
    assert_eq!(v["by_model"][0]["model"], "claude-haiku-4-5");
    assert_eq!(v["by_model"][0]["spent_usd"], 2.5);

    let o = sb(&home, &["budget", "--by-model"]);
    let out = String::from_utf8_lossy(&o.stdout);
    for want in [
        "Summarization spend (estimated",
        "Weeks",
        "Months",
        "$2.00",
        "$0.50",
        "claude-haiku-4-5",
        "3.8M in",
    ] {
        assert!(out.contains(want), "{want:?} missing in:\n{out}");
    }

    // `sb stats` carries the short form.
    let v = json_of(&sb(&home, &["stats", "--json"]));
    assert_eq!(v["budget"]["weekly"]["spent_usd"], 0.5);
    assert_eq!(v["budget"]["weekly"]["cap_usd"], 2.0);
    assert!(v["budget"]["monthly"]["resets_at"].is_string());
    let o = sb(&home, &["stats"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("Summarization budget"));

    // `sb doctor` warns when a cap is reached, and is quiet otherwise.
    let check = |home: &Path| {
        json_of(&sb(home, &["doctor", "--json"]))["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "llm.budget")
            .unwrap_or_else(|| panic!("llm.budget check missing"))
            .clone()
    };
    assert_eq!(check(&home)["status"], "ok");
    assert!(
        sb(
            &home,
            &["config", "set", "summary.budget.weekly_usd", "0.5"]
        )
        .status
        .success()
    );
    assert_eq!(check(&home)["status"], "warning");
    // A disabled cap shows as such and never warns.
    assert!(
        sb(&home, &["config", "set", "summary.budget.weekly_usd", "0"])
            .status
            .success()
    );
    let c = check(&home);
    assert_eq!(c["status"], "ok");
    assert!(c["message"].as_str().unwrap().contains("no cap"));
    let v = json_of(&sb(&home, &["budget", "--json"]));
    assert!(v["current"]["week"]["cap_usd"].is_null());
}
