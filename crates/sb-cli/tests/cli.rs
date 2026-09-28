//! End-to-end tests of the `sb` binary (no network access).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output};

use sb_core::{AccountId, AccountKind};
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
    assert_eq!(v["status"], "warning");
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
