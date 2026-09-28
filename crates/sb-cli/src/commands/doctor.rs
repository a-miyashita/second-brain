//! `sb doctor` (docs/specs/doctor.md).

use std::collections::HashSet;

use chrono::Duration as ChronoDuration;
use sb_core::{
    AccountStatus, RawStatus, RunStatus, RunTrigger, Severity, SourceKind, SummaryStatus,
};
use sb_store::{Catalog, EntryFilter, SCHEMA_VERSION, perms, rawstore};
use serde::Serialize;
use serde_json::{Value, json};

use crate::Ctx;
use crate::cli::DoctorArgs;
use crate::commands::admin::online_check;
use crate::commands::setup::schedule_spec_from_settings;
use crate::util::exit;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Ok,
    Info,
    Warning,
    Error,
}

#[derive(Debug, Serialize)]
struct Check {
    id: &'static str,
    status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    fixed: bool,
}

#[derive(Default)]
struct Report {
    checks: Vec<Check>,
}

impl Report {
    fn add(
        &mut self,
        id: &'static str,
        status: Status,
        message: Option<String>,
        hint: Option<&str>,
    ) {
        self.checks.push(Check {
            id,
            status,
            message,
            hint: hint.map(str::to_string),
            fixed: false,
        });
    }
    fn ok(&mut self, id: &'static str, message: Option<String>) {
        self.add(id, Status::Ok, message, None);
    }
    fn fixed(&mut self, id: &'static str, message: String) {
        self.checks.push(Check {
            id,
            status: Status::Ok,
            message: Some(message),
            hint: None,
            fixed: true,
        });
    }
}

fn issue_hint(code: &str, account: Option<&str>, entry_uid: Option<&str>) -> Option<String> {
    let acct = account.unwrap_or("<account>");
    Some(match code {
        c if c.starts_with("auth.") => format!("sb auth login {acct}"),
        "llm.local_unreachable" => {
            "start the local LLM server, or check the profile with `sb setup llm`".into()
        }
        "llm.failed" | "llm.bad_output" => "sb summarize --retry-failed".into(),
        "llm.auth" | "llm.config" => {
            "check the summarizer profile and its API key (`sb config set-secret ...`)".into()
        }
        "raw.fetch_failed" => match entry_uid {
            Some(u) => format!("sb refetch --entry {u}"),
            None => "sb refetch --raw-missing".into(),
        },
        c if c.starts_with("sync.") => format!("sb sync --account {acct}"),
        _ => return None,
    })
}

pub async fn run(ctx: &Ctx, a: DoctorArgs) -> anyhow::Result<i32> {
    let mut r = Report::default();
    let home = &ctx.home;

    // home.exists
    if !home.is_initialized() {
        if a.fix {
            sb_setup::home::setup_home(home)?;
            r.fixed("home.exists", format!("created {}", home.root().display()));
        } else {
            r.add(
                "home.exists",
                Status::Error,
                Some(format!("{} is not set up", home.root().display())),
                Some("sb setup home"),
            );
            return finish(ctx, r, vec![]);
        }
    } else {
        let probe = home
            .tmp_dir()
            .join(format!("doctor-{}", std::process::id()));
        let writable =
            std::fs::create_dir_all(home.tmp_dir()).is_ok() && std::fs::write(&probe, b"x").is_ok();
        let _ = std::fs::remove_file(&probe);
        if writable {
            r.ok("home.exists", Some(home.root().display().to_string()));
        } else {
            r.add(
                "home.exists",
                Status::Error,
                Some("the home directory is not writable".into()),
                None,
            );
        }
    }

    // home.permissions
    let db = home.db_path();
    let mut loose = Vec::new();
    let mut targets = vec![(home.root().to_path_buf(), true)];
    for suffix in ["", "-wal", "-shm"] {
        let p = db.with_file_name(format!("{}{suffix}", sb_store::home::DB_FILE));
        if p.exists() {
            targets.push((p, false));
        }
    }
    let mut unknown = false;
    for (p, dir) in &targets {
        match perms::is_private(p, *dir)? {
            Some(true) => {}
            Some(false) => loose.push(p.clone()),
            None => unknown = true,
        }
    }
    if loose.is_empty() {
        r.ok(
            "home.permissions",
            unknown.then(|| "could not be verified on this platform".to_string()),
        );
    } else if a.fix {
        for (p, dir) in targets.iter().filter(|(p, _)| loose.contains(p)) {
            if *dir {
                perms::make_private_dir(p)?
            } else {
                perms::make_private_file(p)?
            }
        }
        r.fixed(
            "home.permissions",
            format!("tightened {} path(s)", loose.len()),
        );
    } else {
        r.add(
            "home.permissions",
            Status::Error,
            Some(format!(
                "readable by others: {}",
                loose
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Some("sb doctor --fix"),
        );
    }

    // db.migrations
    let cat = Catalog::open_no_migrate(home)?;
    let v = cat.schema_version()?;
    if v > SCHEMA_VERSION {
        r.add(
            "db.migrations",
            Status::Error,
            Some(format!(
                "the database schema ({v}) is newer than this binary ({SCHEMA_VERSION})"
            )),
            Some("upgrade second-brain"),
        );
        return finish(ctx, r, vec![]);
    } else if v < SCHEMA_VERSION {
        if a.fix {
            cat.migrate()?;
            r.fixed("db.migrations", format!("migrated {v} -> {SCHEMA_VERSION}"));
        } else {
            r.add(
                "db.migrations",
                Status::Error,
                Some(format!("schema {v}, binary expects {SCHEMA_VERSION}")),
                Some("sb doctor --fix"),
            );
            return finish(ctx, r, vec![]);
        }
    } else {
        r.ok("db.migrations", Some(format!("schema {v}")));
    }

    // db.integrity
    let problems = cat.quick_check()?;
    if problems.is_empty() {
        r.ok("db.integrity", None);
    } else {
        r.add(
            "db.integrity",
            Status::Error,
            Some(problems.join("; ")),
            Some("restore the database from a backup"),
        );
    }

    // db.backup
    let last_backup = cat
        .setting("backup.last_at")?
        .and_then(|v| v.as_str().and_then(sb_core::util::parse_ts));
    match last_backup {
        Some(t) if cat.now() - t < ChronoDuration::days(30) => r.ok("db.backup", Some(format!("last backup {}", sb_core::util::ts(t)))),
        _ => r.add(
            "db.backup",
            Status::Info,
            Some("no backup newer than 30 days is known; the database holds credentials and cannot be rebuilt".into()),
            Some("copy second-brain.db while no sync runs, and keep the copy private; then `sb config set backup.last_at <RFC 3339 time>`"),
        ),
    }

    check_raw(&cat, &a, &mut r)?;
    check_queue(&cat, &mut r)?;

    // accounts.status
    let accounts = cat.accounts()?;
    let reauth: Vec<String> = accounts
        .iter()
        .filter(|x| x.status == AccountStatus::NeedsReauth)
        .map(|x| x.id.to_string())
        .collect();
    if reauth.is_empty() {
        r.ok("accounts.status", None);
    } else {
        let hint = format!("sb auth login {}", reauth[0]);
        r.add(
            "accounts.status",
            Status::Error,
            Some(format!("needs re-authentication: {}", reauth.join(", "))),
            Some(&hint),
        );
    }

    // accounts.online
    if a.online {
        let mut failed = Vec::new();
        for acc in accounts
            .iter()
            .filter(|x| x.status == AccountStatus::Active)
        {
            if let Some(Err(m)) = online_check(&cat, acc).await? {
                failed.push(format!("{}: {m}", acc.id));
            }
        }
        if failed.is_empty() {
            r.ok("accounts.online", None);
        } else {
            r.add(
                "accounts.online",
                Status::Error,
                Some(failed.join("; ")),
                Some("sb auth login <account>"),
            );
        }
    }

    check_llm(&cat, &a, &mut r).await?;
    let scheduled = check_schedule(ctx, &cat, &a, &mut r)?;
    check_runs(&cat, scheduled, &mut r)?;
    check_summaries(&cat, &mut r)?;
    check_skills(&a, &mut r)?;

    // index.consistency
    let (fts, secs) = cat.index_counts()?;
    if fts == secs {
        r.ok("index.consistency", Some(format!("{secs} rows")));
    } else if a.fix {
        drop(cat);
        let n = ctx.pipeline()?.rebuild_index()?;
        r.fixed("index.consistency", format!("rebuilt the index ({n} rows)"));
        return finish_with_catalog(ctx, r);
    } else {
        r.add(
            "index.consistency",
            Status::Warning,
            Some(format!("{fts} index rows for {secs} sections")),
            Some("sb index rebuild"),
        );
    }
    check_disk(&cat, &mut r)?;
    let issues = issues_json(&cat)?;
    finish(ctx, r, issues)
}

fn finish_with_catalog(ctx: &Ctx, mut r: Report) -> anyhow::Result<i32> {
    let cat = Catalog::open(&ctx.home)?;
    check_disk(&cat, &mut r)?;
    let issues = issues_json(&cat)?;
    finish(ctx, r, issues)
}

fn check_raw(cat: &Catalog, a: &DoctorArgs, r: &mut Report) -> anyhow::Result<()> {
    let present = cat.list_entries(&EntryFilter {
        raw_status: vec![RawStatus::Present],
        ascending_id: true,
        ..Default::default()
    })?;
    let mut broken = Vec::new();
    let mut hashed = 0usize;
    for e in &present {
        let rows = cat.raw_objects(e.id)?;
        let mut ok = !rows.is_empty();
        let mut roles: Vec<_> = rows.iter().map(|x| x.role).collect();
        roles.dedup();
        for role in roles {
            let mut seqs: Vec<i64> = rows
                .iter()
                .filter(|x| x.role == role)
                .map(|x| x.seq)
                .collect();
            seqs.sort_unstable();
            if seqs.iter().enumerate().any(|(i, s)| *s != i as i64) {
                ok = false;
            }
        }
        for row in &rows {
            let p = cat.home().resolve_rel(&row.path);
            if !p.is_file() {
                ok = false;
                break;
            }
            // Spot-check hashes on a sample.
            if hashed < 50 && ok {
                hashed += 1;
                let bytes = std::fs::read(&p)?;
                if sb_core::util::sha256_hex(&bytes) != row.sha256 {
                    ok = false;
                }
            }
        }
        if !ok {
            broken.push(e);
        }
    }
    if broken.is_empty() {
        r.ok(
            "raw.consistency",
            Some(format!("{} entries with raw data", present.len())),
        );
    } else if a.fix {
        for e in &broken {
            cat.set_raw_status(e.id, RawStatus::Missing)?;
        }
        r.fixed(
            "raw.consistency",
            format!("marked {} entries as raw missing", broken.len()),
        );
    } else {
        r.add(
            "raw.consistency",
            Status::Warning,
            Some(format!(
                "{} entries have missing, non-contiguous or modified raw files (e.g. {})",
                broken.len(),
                broken[0].entry_uid
            )),
            Some("sb doctor --fix, then sb refetch --raw-missing"),
        );
    }
    let orphans = rawstore::find_orphans(cat.home(), &cat.all_raw_paths()?)?;
    if orphans.is_empty() {
        r.ok("raw.orphans", None);
    } else if a.fix {
        for o in &orphans {
            let _ = std::fs::remove_file(o);
        }
        r.fixed(
            "raw.orphans",
            format!("deleted {} orphaned files", orphans.len()),
        );
    } else {
        r.add(
            "raw.orphans",
            Status::Info,
            Some(format!("{} files left by interrupted runs", orphans.len())),
            Some("sb doctor --fix"),
        );
    }
    Ok(())
}

fn check_queue(cat: &Catalog, r: &mut Report) -> anyhow::Result<()> {
    let q = cat.queue(None, &[])?;
    let week_ago = cat.now() - ChronoDuration::days(7);
    let stale = q.iter().filter(|x| x.enqueued_at < week_ago).count();
    let stuck = q
        .iter()
        .filter(|x| x.attempts >= sb_pipeline::sync::MAX_QUEUE_ATTEMPTS)
        .count();
    if stale == 0 && stuck == 0 {
        r.ok("sync.queue", Some(format!("{} queued", q.len())));
    } else {
        let example = q
            .iter()
            .find(|x| x.attempts >= sb_pipeline::sync::MAX_QUEUE_ATTEMPTS)
            .and_then(|x| x.last_error.clone())
            .map(|e| format!("; last error: {e}"))
            .unwrap_or_default();
        r.add(
            "sync.queue",
            Status::Warning,
            Some(format!(
                "{stale} items older than 7 days, {stuck} at the attempt limit{example}"
            )),
            Some("sb sync (check the account's errors)"),
        );
    }
    Ok(())
}

async fn check_llm(cat: &Catalog, a: &DoctorArgs, r: &mut Report) -> anyhow::Result<()> {
    let mut used: Vec<(String, String)> = Vec::new();
    if let Some(v) = cat
        .setting("summary.profile.default")?
        .and_then(|v| v.as_str().map(str::to_string))
    {
        used.push(("summary.profile.default".into(), v));
    }
    for (k, v) in cat.settings_with_prefix("summary.profile.")? {
        if k != "summary.profile.default"
            && let Some(s) = v.as_str()
        {
            used.push((k, s.to_string()));
        }
    }
    let mut problems = Vec::new();
    let mut ok_profiles = Vec::new();
    let mut seen = HashSet::new();
    for (key, name) in &used {
        if name == sb_llm::NATIVE || !seen.insert(name.clone()) {
            continue;
        }
        let Some(raw) = cat.setting(&format!("llm.profiles.{name}"))? else {
            problems.push(format!(
                "{key} = {name:?}, but llm.profiles.{name} is not defined"
            ));
            continue;
        };
        let profile = match sb_llm::Profile::from_value(name, &raw) {
            Ok(p) => p,
            Err(e) => {
                problems.push(e.to_string());
                continue;
            }
        };
        if let Some(bin) = profile.cli_binary()
            && sb_llm::resolve_program(bin).is_none()
        {
            problems.push(format!("profile {name}: `{bin}` is not on PATH"));
            continue;
        }
        if profile.provider.default_secret().is_some() || profile.secret.is_some() {
            let reference = profile.secret_ref().unwrap_or_default();
            if cat.resolve_secret_ref(&reference)?.is_none()
                && profile.provider != sb_llm::Provider::OpenaiCompatible
            {
                problems.push(format!(
                    "profile {name}: no secret {reference} (and no environment fallback)"
                ));
                continue;
            }
        }
        ok_profiles.push((name.clone(), profile));
    }
    if problems.is_empty() {
        r.ok(
            "llm.profiles",
            Some(if used.is_empty() {
                "no summarizer profile configured (summaries stay pending)".into()
            } else {
                format!("{} profile(s) in use", ok_profiles.len())
            }),
        );
    } else {
        r.add(
            "llm.profiles",
            Status::Error,
            Some(problems.join("; ")),
            Some("sb setup llm"),
        );
    }
    if a.online {
        let mut failed = Vec::new();
        let mut notes = Vec::new();
        for (name, profile) in &ok_profiles {
            let secret = match profile.secret_ref() {
                Some(r) => cat.resolve_secret_ref(&r)?,
                None => None,
            };
            let built = match sb_llm::build(
                name,
                profile,
                &sb_llm::BuildOptions {
                    language: "auto".into(),
                    scratch_dir: cat.home().tmp_dir(),
                    secret,
                },
            ) {
                Ok(b) => b,
                Err(e) => {
                    failed.push(format!("{name}: {e}"));
                    continue;
                }
            };
            match built.prepare().await {
                Ok(Some(t)) => notes.push(format!("{name}: warm-up {:.1}s", t.as_secs_f64())),
                Ok(None) => {}
                Err(e) => {
                    failed.push(format!("{name}: {e}"));
                    continue;
                }
            }
            if let Err(e) = built.test_call().await {
                failed.push(format!("{name}: {e}"));
            }
        }
        if failed.is_empty() {
            r.ok("llm.online", (!notes.is_empty()).then(|| notes.join("; ")));
        } else {
            r.add(
                "llm.online",
                Status::Warning,
                Some(failed.join("; ")),
                Some("sb setup llm"),
            );
        }
    }
    Ok(())
}

/// Returns whether a schedule is registered.
fn check_schedule(
    ctx: &Ctx,
    cat: &Catalog,
    a: &DoctorArgs,
    r: &mut Report,
) -> anyhow::Result<bool> {
    let registered = cat.setting("schedule.registered")?;
    let Some(reg) = registered else {
        r.add(
            "schedule.registered",
            Status::Warning,
            Some("no scheduled sync is registered".into()),
            Some("sb setup schedule"),
        );
        return Ok(false);
    };
    let (spec, mechanism) = schedule_spec_from_settings(ctx, cat, &reg)?;
    let user_home = sb_setup::skills::user_home()?;
    let problems = sb_setup::schedule::check(&spec, mechanism, &user_home);
    if problems.is_empty() {
        r.ok("schedule.registered", Some(format!("{mechanism:?}")));
    } else if a.fix {
        sb_setup::schedule::register(&spec, mechanism, &user_home, &ctx.home.tmp_dir())?;
        r.fixed(
            "schedule.registered",
            "re-registered the scheduled jobs".into(),
        );
    } else {
        r.add(
            "schedule.registered",
            Status::Warning,
            Some(problems.join("; ")),
            Some("sb doctor --fix"),
        );
    }
    Ok(true)
}

fn check_runs(cat: &Catalog, scheduled: bool, r: &mut Report) -> anyhow::Result<()> {
    let runs = cat.runs(Some("sync"), Some(RunTrigger::Schedule), 1)?;
    let Some(last) = runs.first() else {
        r.add(
            "runs.recent",
            if scheduled {
                Status::Warning
            } else {
                Status::Info
            },
            Some("no scheduled sync has run yet".into()),
            Some("check the scheduled task, or run `sb sync`"),
        );
        return Ok(());
    };
    let when = last.finished_at.unwrap_or(last.started_at);
    let age = cat.now() - when;
    let remaining = |s: &Value| {
        format!(
            "{} queued fetches, {} pending summaries",
            s.get("queue_remaining")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            s.get("pending_summaries")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        )
    };
    match last.status {
        RunStatus::Failed => r.add(
            "runs.recent",
            Status::Error,
            Some(format!(
                "the last scheduled sync failed at {}: {}",
                sb_core::util::ts(when),
                last.error.clone().unwrap_or_default()
            )),
            Some("sb sync"),
        ),
        _ if age > ChronoDuration::hours(36) => r.add(
            "runs.recent",
            Status::Warning,
            Some(format!("last scheduled sync {} hours ago", age.num_hours())),
            Some("check the scheduled task"),
        ),
        RunStatus::Interrupted
        | RunStatus::StoppedByLimit
        | RunStatus::Partial
        | RunStatus::Running => r.add(
            "runs.recent",
            Status::Warning,
            Some(format!(
                "last scheduled sync: {}; remaining: {}",
                last.status,
                remaining(&last.stats)
            )),
            Some("sb sync"),
        ),
        RunStatus::Ok => r.ok(
            "runs.recent",
            Some(format!("last scheduled sync {}", sb_core::util::ts(when))),
        ),
    }
    Ok(())
}

fn check_summaries(cat: &Catalog, r: &mut Report) -> anyhow::Result<()> {
    let max: i64 = cat.setting_or("summary.max_attempts", 3i64)?;
    let count = |s: SummaryStatus| -> anyhow::Result<u64> {
        Ok(cat.count_entries(&EntryFilter {
            summary_status: vec![s],
            ..Default::default()
        })?)
    };
    let pending = count(SummaryStatus::Pending)?;
    let failed = count(SummaryStatus::Failed)?;
    let at_limit = cat
        .list_entries(&EntryFilter {
            summary_status: vec![SummaryStatus::Failed],
            ..Default::default()
        })?
        .iter()
        .filter(|e| e.summary_attempts >= max)
        .count();
    let msg = format!("{pending} pending, {failed} failed ({at_limit} at the attempt limit)");
    if failed > 0 {
        r.add(
            "summaries.backlog",
            Status::Warning,
            Some(msg),
            Some("sb summarize --retry-failed"),
        );
    } else if pending > 0 {
        r.add(
            "summaries.backlog",
            Status::Info,
            Some(msg),
            Some("sb summarize"),
        );
    } else {
        r.ok("summaries.backlog", Some(msg));
    }
    // meet.transcripts
    let meets = cat.list_entries(&EntryFilter {
        source_kinds: vec![SourceKind::GoogleMeet],
        raw_status: vec![RawStatus::Present],
        ..Default::default()
    })?;
    if !meets.is_empty() {
        let without = meets
            .iter()
            .filter(|e| e.metadata.get("has_transcript").and_then(Value::as_bool) != Some(true))
            .count();
        r.add(
            "meet.transcripts",
            if without == 0 {
                Status::Ok
            } else {
                Status::Info
            },
            Some(format!(
                "{without} of {} meetings have no transcript",
                meets.len()
            )),
            None,
        );
    }
    Ok(())
}

fn check_skills(a: &DoctorArgs, r: &mut Report) -> anyhow::Result<()> {
    let user_home = sb_setup::skills::user_home()?;
    let want = sb_setup::skills::embedded_version();
    let mut outdated = Vec::new();
    let mut installed = 0;
    for t in sb_setup::skills::Target::ALL {
        if let Some(v) = sb_setup::skills::installed_version(t, &user_home) {
            installed += 1;
            if v != want {
                outdated.push(t);
            }
        }
    }
    if outdated.is_empty() {
        if installed == 0 {
            r.add(
                "skills.version",
                Status::Info,
                Some("no agent skill is installed".into()),
                Some("sb setup skills"),
            );
        } else {
            r.ok("skills.version", Some(format!("version {want}")));
        }
    } else if a.fix {
        for t in &outdated {
            sb_setup::skills::install(*t, &user_home)?;
        }
        r.fixed(
            "skills.version",
            format!("reinstalled {} skill(s)", outdated.len()),
        );
    } else {
        r.add(
            "skills.version",
            Status::Warning,
            Some(format!(
                "outdated for {} (binary has version {want})",
                outdated
                    .iter()
                    .map(|t| t.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Some("sb doctor --fix"),
        );
    }
    Ok(())
}

fn check_disk(cat: &Catalog, r: &mut Report) -> anyhow::Result<()> {
    let size = rawstore::dir_size(cat.home().root())?;
    let free = fs4::available_space(cat.home().root()).ok();
    let msg = format!(
        "home uses {:.1} MB{}",
        size as f64 / 1e6,
        free.map(|f| format!(", {:.1} GB free", f as f64 / 1e9))
            .unwrap_or_default()
    );
    if free.is_some_and(|f| f < 1_000_000_000) {
        r.add(
            "disk.usage",
            Status::Warning,
            Some(msg),
            Some("free disk space"),
        );
    } else {
        r.ok("disk.usage", Some(msg));
    }
    Ok(())
}

fn issues_json(cat: &Catalog) -> anyhow::Result<Vec<Value>> {
    let mut out = Vec::new();
    for i in cat.open_issues()? {
        let uid = match i.entry_id {
            Some(id) => cat.entry(id)?.map(|e| e.entry_uid),
            None => None,
        };
        out.push(json!({
            "code": i.code,
            "severity": i.severity,
            "account": i.account_id,
            "entry_uid": uid,
            "message": i.message,
            "first_seen_at": sb_core::util::ts(i.first_seen_at),
            "last_seen_at": sb_core::util::ts(i.last_seen_at),
            "hint": issue_hint(&i.code, i.account_id.as_deref(), uid.as_deref()),
        }));
    }
    Ok(out)
}

fn finish(ctx: &Ctx, r: Report, issues: Vec<Value>) -> anyhow::Result<i32> {
    let sev = |s: &str| match s {
        "error" => Status::Error,
        "warning" => Status::Warning,
        _ => Status::Info,
    };
    let worst = r
        .checks
        .iter()
        .map(|c| c.status)
        .chain(
            issues
                .iter()
                .map(|i| sev(i["severity"].as_str().unwrap_or(""))),
        )
        .fold(Status::Ok, |a, b| match (a, b) {
            (Status::Error, _) | (_, Status::Error) => Status::Error,
            (Status::Warning, _) | (_, Status::Warning) => Status::Warning,
            _ => Status::Ok,
        });
    if ctx.json {
        ctx.out_json(
            "sb.doctor/v1",
            json!({"status": worst, "checks": r.checks, "issues": issues}),
        );
    } else {
        for c in &r.checks {
            let mark = match c.status {
                Status::Ok if c.fixed => "✔ (fixed)",
                Status::Ok => "✔",
                Status::Info => "·",
                Status::Warning => "!",
                Status::Error => "✖",
            };
            let msg = c
                .message
                .as_deref()
                .map(|m| format!(": {m}"))
                .unwrap_or_default();
            println!("{mark} {}{msg}", c.id);
            if let Some(h) = &c.hint
                && c.status != Status::Ok
            {
                println!("    -> {h}");
            }
        }
        if !issues.is_empty() {
            println!("\nOpen issues:");
            for s in [Severity::Error, Severity::Warning, Severity::Info] {
                for i in issues.iter().filter(|i| i["severity"] == s.as_str()) {
                    println!(
                        "  [{}] {}{}: {} (since {}, last {})",
                        s,
                        i["code"].as_str().unwrap_or(""),
                        i["account"]
                            .as_str()
                            .map(|a| format!(" ({a})"))
                            .unwrap_or_default(),
                        i["message"].as_str().unwrap_or(""),
                        i["first_seen_at"].as_str().unwrap_or(""),
                        i["last_seen_at"].as_str().unwrap_or("")
                    );
                    if let Some(h) = i["hint"].as_str() {
                        println!("      -> {h}");
                    }
                }
            }
        }
    }
    Ok(match worst {
        Status::Error => exit::FAILURE,
        Status::Warning => exit::PROBLEMS,
        _ => exit::OK,
    })
}
