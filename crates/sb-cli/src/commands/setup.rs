//! `sb setup` wizard and steps (docs/specs/setup-and-scheduling.md).

use std::path::PathBuf;

use sb_core::Secret;
use sb_setup::schedule::{self, Mechanism, ScheduleSpec, TimeOfDay, Weekday};
use sb_setup::skills::{self, Target};
use sb_store::{Catalog, SecretScope};
use serde_json::{Value, json};

use crate::Ctx;
use crate::cli::{SetupCmd, SetupLlmArgs, SetupScheduleArgs, SetupSkillsArgs, SetupStep};
use crate::util::{self, exit, usage};

/// The `second-brain` binary to register (the `sb` alias maps to its sibling).
pub fn main_binary() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let stem = exe.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if stem == "sb" {
        let sibling = exe.with_file_name(format!("second-brain{}", std::env::consts::EXE_SUFFIX));
        if sibling.is_file() {
            return Ok(sibling);
        }
    }
    Ok(exe)
}

/// Executables the scheduled jobs need on `PATH`: LLM CLIs and local
/// `start_command`s of the configured profiles.
fn llm_executables(cat: &Catalog) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for (key, v) in cat.settings_with_prefix("llm.profiles.")? {
        let name = key.trim_start_matches("llm.profiles.");
        let Ok(p) = sb_llm::Profile::from_value(name, &v) else {
            continue;
        };
        if let Some(b) = p.cli_binary().and_then(sb_llm::resolve_program) {
            out.push(b);
        }
        if let Some(cmd) = p
            .start_command
            .as_ref()
            .and_then(|c| c.first())
            .and_then(|c| sb_llm::resolve_program(c))
        {
            out.push(cmd);
        }
    }
    Ok(out)
}

/// Rebuild the schedule spec from the `schedule.registered` setting.
pub fn schedule_spec_from_settings(
    ctx: &Ctx,
    cat: &Catalog,
    reg: &Value,
) -> anyhow::Result<(ScheduleSpec, Mechanism)> {
    let s = |k: &str, d: &str| reg.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
    let mechanism: Mechanism =
        serde_json::from_value(reg.get("mechanism").cloned().unwrap_or(Value::Null))
            .unwrap_or(Mechanism::for_os(false));
    let spec = ScheduleSpec {
        binary: main_binary()?,
        home: ctx.home.root().to_path_buf(),
        daily: TimeOfDay::parse(&s("time", "19:30")).unwrap_or(TimeOfDay {
            hour: 19,
            minute: 30,
        }),
        deep_day: Weekday::parse(&s("deep_day", "mon")).unwrap_or(Weekday::Mon),
        deep_time: TimeOfDay::parse(&s("deep_time", "18:30")).unwrap_or(TimeOfDay {
            hour: 18,
            minute: 30,
        }),
        path: schedule::job_path(&llm_executables(cat)?),
    };
    Ok((spec, mechanism))
}

pub async fn run(ctx: &Ctx, cmd: SetupCmd) -> anyhow::Result<i32> {
    match cmd.step {
        None => wizard(ctx, cmd.yes).await,
        Some(SetupStep::Home) => setup_home(ctx),
        Some(SetupStep::Llm(a)) => setup_llm(ctx, a, cmd.yes).await,
        Some(SetupStep::Schedule(a)) => setup_schedule(ctx, a),
        Some(SetupStep::Skills(a)) => setup_skills(ctx, a),
        Some(SetupStep::Env) => setup_env(ctx, cmd.yes),
    }
}

fn setup_home(ctx: &Ctx) -> anyhow::Result<i32> {
    let (_, report) = sb_setup::home::setup_home(&ctx.home)?;
    if ctx.json {
        ctx.out_json("sb.setup/v1", json!({"step": "home", "report": report}));
    } else {
        eprintln!(
            "{} {}",
            if report.created { "Created" } else { "Checked" },
            report.home
        );
        if !report.default_home {
            eprintln!(
                "This is not the default location; run `sb setup env` so scheduled jobs and agents find it."
            );
        }
    }
    Ok(exit::OK)
}

async fn setup_llm(ctx: &Ctx, a: SetupLlmArgs, yes: bool) -> anyhow::Result<i32> {
    let cat = ctx.catalog()?;
    let presets = sb_setup::llm::presets();
    let preset = match &a.preset {
        Some(k) => presets
            .iter()
            .find(|p| p.key == k)
            .ok_or_else(|| usage(format!("unknown preset {k:?}")))?
            .clone(),
        None if yes || !util::interactive() => {
            return Err(usage(
                "give --preset (anthropic, openai, claude_cli, copilot_cli, local)",
            ));
        }
        None => {
            let mut labels: Vec<String> = presets.iter().map(|p| p.label.to_string()).collect();
            labels.push("None (configure later)".into());
            let i = util::choose(
                "Summarizer for Slack threads and meeting re-summaries:",
                &labels,
                0,
            )?;
            if i == presets.len() {
                return Ok(exit::OK);
            }
            presets[i].clone()
        }
    };
    let name = a.name.clone().unwrap_or_else(|| preset.key.to_string());
    let mut profile = preset.profile.clone();
    let interactive = util::interactive() && !yes;
    let default_model = profile
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let model = match (&a.model, interactive) {
        (Some(m), _) => m.clone(),
        (None, true) => util::ask("Model (empty = provider default)", Some(&default_model))?,
        (None, false) => default_model,
    };
    if model.is_empty() {
        if let Some(o) = profile.as_object_mut() {
            o.remove("model");
        }
    } else {
        profile["model"] = json!(model);
    }
    if preset.key == "local" {
        let default_url = profile["base_url"].as_str().unwrap_or("").to_string();
        let url = match (&a.base_url, interactive) {
            (Some(u), _) => u.clone(),
            (None, true) => util::ask(
                "Base URL of the OpenAI-compatible server",
                Some(&default_url),
            )?,
            (None, false) => default_url,
        };
        profile["base_url"] = json!(url);
    } else if let Some(u) = &a.base_url {
        profile["base_url"] = json!(u);
    }
    if let Some(bin) = preset.binary
        && sb_llm::resolve_program(bin).is_none()
    {
        eprintln!("warning: `{bin}` was not found on PATH");
    }
    let p = sb_llm::Profile::from_value(&name, &profile).map_err(|e| usage(e.to_string()))?;
    if let Some(secret) = preset.secret
        && cat.secret_or_env(&SecretScope::Global, secret)?.is_none()
    {
        if interactive {
            let v = util::read_secret(&format!("API key ({secret})"))?;
            cat.set_secret(&SecretScope::Global, secret, &Secret::new(v))?;
        } else {
            eprintln!("warning: no {secret}; set it with `sb config set-secret {secret}`");
        }
    }
    cat.set_setting(&format!("llm.profiles.{name}"), &profile)?;
    if !a.no_default {
        cat.set_setting("summary.profile.default", &json!(name))?;
    }
    let mut test = Value::Null;
    if !a.no_test {
        let secret = match p.secret_ref() {
            Some(r) => cat.resolve_secret_ref(&r)?,
            None => None,
        };
        let started = std::time::Instant::now();
        let result = async {
            let built = sb_llm::build(
                &name,
                &p,
                &sb_llm::BuildOptions {
                    language: "auto".into(),
                    scratch_dir: ctx.home.tmp_dir(),
                    secret,
                },
            )?;
            built.prepare().await?;
            built.test_call().await
        }
        .await;
        test = match &result {
            Ok(t) => json!({"ok": true, "reply": t, "ms": started.elapsed().as_millis() as u64}),
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        };
        if !ctx.json {
            match result {
                Ok(_) => eprintln!("Test call OK ({:.1}s).", started.elapsed().as_secs_f64()),
                Err(e) => eprintln!("Test call failed: {e}"),
            }
        }
    }
    if ctx.json {
        ctx.out_json(
            "sb.setup/v1",
            json!({"step": "llm", "profile": name, "config": profile, "test": test}),
        );
    } else {
        eprintln!(
            "Saved profile {name}{}.",
            if a.no_default { "" } else { " as the default" }
        );
    }
    Ok(if test.get("ok") == Some(&json!(false)) {
        exit::PROBLEMS
    } else {
        exit::OK
    })
}

fn setup_schedule(ctx: &Ctx, a: SetupScheduleArgs) -> anyhow::Result<i32> {
    let cat = ctx.catalog()?;
    let user_home = skills::user_home()?;
    let reg = json!({
        "mechanism": Mechanism::for_os(a.systemd),
        "time": a.time, "deep_day": a.deep_day, "deep_time": a.deep_time,
    });
    TimeOfDay::parse(&a.time).ok_or_else(|| usage(format!("invalid --time {:?}", a.time)))?;
    TimeOfDay::parse(&a.deep_time)
        .ok_or_else(|| usage(format!("invalid --deep-time {:?}", a.deep_time)))?;
    Weekday::parse(&a.deep_day)
        .ok_or_else(|| usage(format!("invalid --deep-day {:?}", a.deep_day)))?;
    let (spec, mechanism) = schedule_spec_from_settings(ctx, &cat, &reg)?;
    if a.remove {
        let registered = cat
            .setting("schedule.registered")?
            .and_then(|r| {
                serde_json::from_value::<Mechanism>(
                    r.get("mechanism").cloned().unwrap_or(Value::Null),
                )
                .ok()
            })
            .unwrap_or(mechanism);
        if !a.dry_run {
            schedule::unregister(registered, &user_home)?;
            cat.unset_setting("schedule.registered")?;
        }
        if ctx.json {
            ctx.out_json(
                "sb.setup/v1",
                json!({"step": "schedule", "removed": true, "mechanism": registered}),
            );
        } else {
            eprintln!("Removed the scheduled jobs ({registered:?}).");
        }
        return Ok(exit::OK);
    }
    let rendered = schedule::render_all(&spec, mechanism);
    if a.dry_run {
        if ctx.json {
            ctx.out_json(
                "sb.setup/v1",
                json!({"step": "schedule", "dry_run": true, "mechanism": mechanism,
                       "files": rendered.iter().map(|(n, c)| json!({"name": n, "content": c})).collect::<Vec<_>>()}),
            );
        } else {
            for (n, c) in &rendered {
                println!("# {n}\n{c}");
            }
        }
        return Ok(exit::OK);
    }
    schedule::register(&spec, mechanism, &user_home, &ctx.home.tmp_dir())?;
    let mut stored = reg.clone();
    stored["binary"] = json!(spec.binary.display().to_string());
    stored["registered_at"] = json!(sb_core::util::ts(cat.now()));
    cat.set_setting("schedule.registered", &stored)?;
    if ctx.json {
        ctx.out_json(
            "sb.setup/v1",
            json!({"step": "schedule", "mechanism": mechanism, "registered": stored}),
        );
    } else {
        eprintln!(
            "Registered daily sync at {} and deep sync on {} at {} ({mechanism:?}).",
            a.time, a.deep_day, a.deep_time
        );
    }
    Ok(exit::OK)
}

fn setup_skills(ctx: &Ctx, a: SetupSkillsArgs) -> anyhow::Result<i32> {
    let targets = Target::parse(&a.target)
        .ok_or_else(|| usage("--target is copilot, claude, codex or all"))?;
    let user_home = skills::user_home()?;
    let mut done = Vec::new();
    for t in targets {
        if a.remove {
            if skills::remove(t, &user_home)? {
                done.push(json!({"target": t.name(), "removed": true}));
            }
        } else {
            let dir = skills::install(t, &user_home)?;
            done.push(json!({"target": t.name(), "path": dir.display().to_string()}));
        }
    }
    if let Ok(cat) = ctx.catalog() {
        let installed: Vec<&str> = Target::ALL
            .iter()
            .filter(|t| skills::installed_version(**t, &user_home).is_some())
            .map(|t| t.name())
            .collect();
        cat.set_setting("skills.installed", &json!(installed))?;
    }
    if ctx.json {
        ctx.out_json(
            "sb.setup/v1",
            json!({"step": "skills", "version": skills::embedded_version(), "results": done}),
        );
    } else {
        for d in &done {
            match d.get("path") {
                Some(p) => eprintln!(
                    "Installed the skill for {} at {}",
                    d["target"].as_str().unwrap_or(""),
                    p.as_str().unwrap_or("")
                ),
                None => eprintln!(
                    "Removed the skill for {}",
                    d["target"].as_str().unwrap_or("")
                ),
            }
        }
    }
    Ok(exit::OK)
}

fn setup_env(ctx: &Ctx, yes: bool) -> anyhow::Result<i32> {
    let home = ctx.home.root();
    if ctx.home.is_default() {
        if !ctx.json {
            eprintln!("{} is the default home; nothing to do.", home.display());
        } else {
            ctx.out_json("sb.setup/v1", json!({"step": "env", "needed": false}));
        }
        return Ok(exit::OK);
    }
    if cfg!(windows) {
        sb_setup::env::set_windows_user_env(home)?;
        if ctx.json {
            ctx.out_json(
                "sb.setup/v1",
                json!({"step": "env", "set": "HKCU\\Environment\\SECOND_BRAIN_HOME"}),
            );
        } else {
            eprintln!("Set SECOND_BRAIN_HOME for your user. Open a new terminal to use it.");
        }
        return Ok(exit::OK);
    }
    let shell = std::env::var("SHELL").unwrap_or_default();
    let (rc, line) = sb_setup::env::shell_rc(&skills::user_home()?, &shell, home);
    let write = yes
        || (util::interactive()
            && util::confirm(&format!("Add `{line}` to {}?", rc.display()), true)?);
    if write {
        sb_setup::env::write_rc(&rc, &line)?;
    }
    if ctx.json {
        ctx.out_json(
            "sb.setup/v1",
            json!({"step": "env", "line": line, "rc": rc.display().to_string(), "written": write}),
        );
    } else if write {
        eprintln!("Added to {}. Open a new shell to use it.", rc.display());
    } else {
        println!("{line}");
    }
    Ok(exit::OK)
}

async fn wizard(ctx: &Ctx, yes: bool) -> anyhow::Result<i32> {
    if ctx.json {
        return Err(usage(
            "the setup wizard is interactive; run the steps (`sb setup home`, ...) for JSON output",
        ));
    }
    let interactive = util::interactive() && !yes;
    eprintln!("== 1/6 Home: {}", ctx.home.root().display());
    setup_home(ctx)?;
    if !ctx.home.is_default()
        && (yes
            || (interactive && util::confirm("Persist SECOND_BRAIN_HOME (sb setup env)?", true)?))
    {
        setup_env(ctx, yes)?;
    }

    eprintln!("\n== 2/6 Accounts");
    if interactive {
        loop {
            let i = util::choose(
                "Add an account:",
                &[
                    "Google account (Meet notes)".into(),
                    "Slack workspace".into(),
                    "Done".into(),
                ],
                2,
            )?;
            let r = match i {
                0 => {
                    eprintln!(
                        "You need an OAuth client of type \"Desktop app\" (see the README guide)."
                    );
                    let id = util::ask("Account ID (e.g. work-google)", None)?;
                    let file = util::ask("Path to the client JSON (or `global`)", None)?;
                    super::admin::account(
                        ctx,
                        crate::cli::AccountCmd::Add(crate::cli::AccountAdd::Google {
                            id,
                            client_secret: file,
                            label: None,
                            features: vec!["meet".into()],
                            no_browser: false,
                        }),
                    )
                    .await
                }
                1 => {
                    eprintln!(
                        "Create a Slack app from assets/slack-app-manifest.yaml, install it, and copy the User OAuth Token."
                    );
                    let id = util::ask("Account ID (e.g. acme-slack)", None)?;
                    super::admin::account(
                        ctx,
                        crate::cli::AccountCmd::Add(crate::cli::AccountAdd::Slack {
                            id,
                            token: None,
                            label: None,
                        }),
                    )
                    .await
                }
                _ => break,
            };
            if let Err(e) = r {
                eprintln!("error: {e:#}");
            }
        }
    } else {
        eprintln!("Skipped (non-interactive). Use `sb account add google|slack`.");
    }

    eprintln!("\n== 3/6 Summarizer");
    if interactive {
        if let Err(e) = setup_llm(
            ctx,
            SetupLlmArgs {
                preset: None,
                name: None,
                model: None,
                base_url: None,
                no_default: false,
                no_test: false,
            },
            false,
        )
        .await
        {
            eprintln!("error: {e:#}");
        }
    } else {
        eprintln!("Skipped (non-interactive). Use `sb setup llm --preset ...`.");
    }

    eprintln!("\n== 4/6 Schedule");
    let mut sched = SetupScheduleArgs {
        time: "19:30".into(),
        deep_day: "mon".into(),
        deep_time: "18:30".into(),
        systemd: false,
        remove: false,
        dry_run: false,
    };
    if interactive {
        sched.time = util::ask("Daily sync time", Some("19:30"))?;
        sched.deep_day = util::ask("Weekly deep sync day", Some("mon"))?;
        sched.deep_time = util::ask("Weekly deep sync time", Some("18:30"))?;
    }
    if (yes || (interactive && util::confirm("Register the scheduled jobs?", true)?))
        && let Err(e) = setup_schedule(ctx, sched)
    {
        eprintln!("error: {e:#}");
    }

    eprintln!("\n== 5/6 Agent skills");
    for t in Target::ALL {
        if !t.is_installed_agent() {
            continue;
        }
        if yes
            || (interactive
                && util::confirm(
                    &format!("Install the skill for {} ({})?", t.name(), t.binary()),
                    true,
                )?)
        {
            setup_skills(
                ctx,
                SetupSkillsArgs {
                    target: t.name().into(),
                    remove: false,
                },
            )?;
        }
    }

    eprintln!("\n== 6/6 First sync");
    {
        // The first window applies to every source (ADR-0016).
        let cat = ctx.catalog()?;
        let current: u64 = cat.setting_or("sync.initial_days", 30u64)?;
        if interactive {
            let answer = util::ask(
                "How many days back should the first sync reach (Slack, Meet)?",
                Some(&current.to_string()),
            )?;
            let days: u64 = answer
                .trim()
                .parse()
                .ok()
                .filter(|d| *d > 0)
                .ok_or_else(|| util::usage("the number of days is a positive whole number"))?;
            if days != current {
                cat.set_setting("sync.initial_days", &json!(days))?;
            }
            eprintln!("The first sync reaches back {days} days.");
        } else {
            eprintln!("The first sync reaches back {current} days (sync.initial_days).");
        }
        eprintln!(
            "To go further back later: `sb sync --since 90d` fetches only what is older than what is already synced."
        );
    }
    if interactive && util::confirm("Estimate the first sync now (`sb sync --estimate`)?", true)? {
        let code = super::ingest::sync(
            ctx,
            crate::cli::SyncArgs {
                accounts: vec![],
                sources: vec![],
                deep: false,
                since: None,
                until: None,
                no_summary: false,
                limits: Default::default(),
                dry_run: false,
                estimate: true,
            },
        )
        .await?;
        if code == exit::OK
            && util::confirm(
                "Run `sb sync` now? (It can be stopped with Ctrl+C and resumed later.)",
                false,
            )?
        {
            return super::ingest::sync(
                ctx,
                crate::cli::SyncArgs {
                    accounts: vec![],
                    sources: vec![],
                    deep: false,
                    since: None,
                    until: None,
                    no_summary: false,
                    limits: Default::default(),
                    dry_run: false,
                    estimate: false,
                },
            )
            .await;
        }
    }
    eprintln!("\nSetup finished. `sb doctor` shows what still needs attention.");
    Ok(exit::OK)
}
