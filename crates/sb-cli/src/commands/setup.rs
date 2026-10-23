//! `sb setup` steps and the non-interactive `--yes` run
//! (docs/specs/setup-and-scheduling.md). The setup screen is in `crate::tui`.

use std::path::PathBuf;

use second_brain_kernel::Secret;
use second_brain_setup::llm::Preset;
use second_brain_setup::schedule::{self, Mechanism, ScheduleSpec, TimeOfDay, Weekday};
use second_brain_setup::skills::{self, Target};
use second_brain_store::{Catalog, SecretScope};
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
        let Ok(p) = second_brain_llm::Profile::from_value(name, &v) else {
            continue;
        };
        if let Some(b) = p.cli_binary().and_then(second_brain_llm::resolve_program) {
            out.push(b);
        }
        if let Some(cmd) = p
            .start_command
            .as_ref()
            .and_then(|c| c.first())
            .and_then(|c| second_brain_llm::resolve_program(c))
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
        None if ctx.json => Err(usage(
            "the setup screen is interactive; run the steps (`sb setup home`, ...) for JSON output",
        )),
        None if cmd.yes => defaults(ctx),
        None => crate::tui::run(ctx, crate::tui::Start::Setup).await,
        Some(SetupStep::Home) => setup_home(ctx),
        Some(SetupStep::Llm(a)) => setup_llm(ctx, a, cmd.yes).await,
        Some(SetupStep::Schedule(a)) => setup_schedule(ctx, a),
        Some(SetupStep::Skills(a)) => setup_skills(ctx, a),
        Some(SetupStep::Env) => setup_env(ctx, cmd.yes),
    }
}

fn setup_home(ctx: &Ctx) -> anyhow::Result<i32> {
    let (_, report) = second_brain_setup::home::setup_home(&ctx.home)?;
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

/// The profile JSON of `preset`. `model: Some("")` removes the model (provider
/// default); `None` keeps the preset's.
pub(crate) fn preset_profile(
    preset: &Preset,
    model: Option<&str>,
    base_url: Option<&str>,
) -> Value {
    let mut profile = preset.profile.clone();
    match model {
        Some("") => {
            if let Some(o) = profile.as_object_mut() {
                o.remove("model");
            }
        }
        Some(m) => profile["model"] = json!(m),
        None => {}
    }
    if let Some(u) = base_url {
        profile["base_url"] = json!(u);
    }
    profile
}

/// The outcome of saving a summarizer profile.
pub(crate) struct LlmReport {
    pub name: String,
    pub profile: Value,
    /// `null` when the test was skipped, else `{"ok", ...}`.
    pub test: Value,
    pub warnings: Vec<String>,
}

/// Validate and store profile `name`, optionally make it the default and test it.
pub(crate) async fn save_llm(
    ctx: &Ctx,
    cat: &Catalog,
    name: &str,
    preset: &Preset,
    profile: Value,
    make_default: bool,
    test: bool,
) -> anyhow::Result<LlmReport> {
    let mut warnings = Vec::new();
    if let Some(bin) = preset.binary
        && second_brain_llm::resolve_program(bin).is_none()
    {
        warnings.push(format!("`{bin}` was not found on PATH"));
    }
    let p =
        second_brain_llm::Profile::from_value(name, &profile).map_err(|e| usage(e.to_string()))?;
    if let Some(secret) = preset.secret
        && cat.secret_or_env(&SecretScope::Global, secret)?.is_none()
    {
        warnings.push(format!(
            "no {secret}; set it with `sb config set-secret {secret}`"
        ));
    }
    cat.set_setting(&format!("llm.profiles.{name}"), &profile)?;
    if make_default {
        cat.set_setting("summary.profile.default", &json!(name))?;
    }
    let mut result = Value::Null;
    if test {
        let secret = match p.secret_ref() {
            Some(r) => cat.resolve_secret_ref(&r)?,
            None => None,
        };
        let started = std::time::Instant::now();
        let r = async {
            let built = second_brain_llm::build(
                name,
                &p,
                &second_brain_llm::BuildOptions {
                    language: "auto".into(),
                    scratch_dir: ctx.home.tmp_dir(),
                    secret,
                },
            )?;
            built.prepare().await?;
            built.test_call().await
        }
        .await;
        result = match &r {
            Ok(t) => json!({"ok": true, "reply": t, "ms": started.elapsed().as_millis() as u64}),
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        };
    }
    Ok(LlmReport {
        name: name.to_string(),
        profile,
        test: result,
        warnings,
    })
}

async fn setup_llm(ctx: &Ctx, a: SetupLlmArgs, yes: bool) -> anyhow::Result<i32> {
    let cat = ctx.catalog()?;
    let presets = second_brain_setup::llm::presets();
    let preset = match &a.preset {
        Some(k) => presets
            .iter()
            .find(|p| p.key == k)
            .ok_or_else(|| usage(format!("unknown preset {k:?}")))?
            .clone(),
        None if yes || !util::interactive() => {
            return Err(usage(
                "give --preset (anthropic, openai, claude_cli, copilot_cli, codex_cli, antigravity_cli, local)",
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
    let interactive = util::interactive() && !yes;
    let default_model = preset
        .profile
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let model = match (&a.model, interactive) {
        (Some(m), _) => m.clone(),
        (None, true) => util::ask("Model (empty = provider default)", Some(&default_model))?,
        (None, false) => default_model,
    };
    let base_url = if preset.key == "local" {
        let default_url = preset.profile["base_url"]
            .as_str()
            .unwrap_or("")
            .to_string();
        Some(match (&a.base_url, interactive) {
            (Some(u), _) => u.clone(),
            (None, true) => util::ask(
                "Base URL of the OpenAI-compatible server",
                Some(&default_url),
            )?,
            (None, false) => default_url,
        })
    } else {
        a.base_url.clone()
    };
    let profile = preset_profile(&preset, Some(&model), base_url.as_deref());
    if interactive
        && let Some(secret) = preset.secret
        && cat.secret_or_env(&SecretScope::Global, secret)?.is_none()
    {
        let v = util::read_secret(&format!("API key ({secret})"))?;
        cat.set_secret(&SecretScope::Global, secret, &Secret::new(v))?;
    }
    let r = save_llm(
        ctx,
        &cat,
        &name,
        &preset,
        profile,
        !a.no_default,
        !a.no_test,
    )
    .await?;
    if ctx.json {
        ctx.out_json(
            "sb.setup/v1",
            json!({"step": "llm", "profile": r.name, "config": r.profile, "test": r.test}),
        );
    } else {
        for w in &r.warnings {
            eprintln!("warning: {w}");
        }
        if let Some(line) = test_message(&r.test) {
            eprintln!("{line}");
        }
        eprintln!(
            "Saved profile {name}{}.",
            if a.no_default { "" } else { " as the default" }
        );
    }
    Ok(if r.test.get("ok") == Some(&json!(false)) {
        exit::PROBLEMS
    } else {
        exit::OK
    })
}

/// One line describing a test call result, or `None` when it was skipped.
pub(crate) fn test_message(test: &Value) -> Option<String> {
    match test.get("ok").and_then(Value::as_bool)? {
        true => Some(format!(
            "Test call OK ({:.1}s).",
            test["ms"].as_u64().unwrap_or(0) as f64 / 1000.0
        )),
        false => Some(format!(
            "Test call failed: {}",
            test["error"].as_str().unwrap_or("")
        )),
    }
}

/// The mechanism of the registered jobs (`schedule.registered`), if any.
fn registered_mechanism(cat: &Catalog) -> anyhow::Result<Option<Mechanism>> {
    Ok(cat.setting("schedule.registered")?.and_then(|r| {
        serde_json::from_value::<Mechanism>(r.get("mechanism").cloned().unwrap_or(Value::Null)).ok()
    }))
}

fn check_schedule_args(a: &SetupScheduleArgs) -> anyhow::Result<()> {
    TimeOfDay::parse(&a.time).ok_or_else(|| usage(format!("invalid --time {:?}", a.time)))?;
    TimeOfDay::parse(&a.deep_time)
        .ok_or_else(|| usage(format!("invalid --deep-time {:?}", a.deep_time)))?;
    Weekday::parse(&a.deep_day)
        .ok_or_else(|| usage(format!("invalid --deep-day {:?}", a.deep_day)))?;
    Ok(())
}

/// Register (or, with `a.remove`, remove) the scheduled jobs. Returns the
/// `sb.setup/v1` report and a message. `a.dry_run` is not handled here.
pub(crate) fn apply_schedule(
    ctx: &Ctx,
    cat: &Catalog,
    a: &SetupScheduleArgs,
) -> anyhow::Result<(Value, String)> {
    let user_home = skills::user_home()?;
    let reg = json!({
        "mechanism": Mechanism::for_os(a.systemd),
        "time": a.time, "deep_day": a.deep_day, "deep_time": a.deep_time,
    });
    check_schedule_args(a)?;
    let (spec, mechanism) = schedule_spec_from_settings(ctx, cat, &reg)?;
    if a.remove {
        let registered = registered_mechanism(cat)?.unwrap_or(mechanism);
        schedule::unregister(registered, &user_home)?;
        cat.unset_setting("schedule.registered")?;
        return Ok((
            json!({"step": "schedule", "removed": true, "mechanism": registered}),
            format!("Removed the scheduled jobs ({registered:?})."),
        ));
    }
    schedule::register(&spec, mechanism, &user_home, &ctx.home.tmp_dir())?;
    let mut stored = reg.clone();
    stored["binary"] = json!(spec.binary.display().to_string());
    stored["registered_at"] = json!(second_brain_kernel::util::ts(cat.now()));
    cat.set_setting("schedule.registered", &stored)?;
    Ok((
        json!({"step": "schedule", "mechanism": mechanism, "registered": stored}),
        format!(
            "Registered daily sync at {} and deep sync on {} at {} ({mechanism:?}).",
            a.time, a.deep_day, a.deep_time
        ),
    ))
}

fn setup_schedule(ctx: &Ctx, a: SetupScheduleArgs) -> anyhow::Result<i32> {
    let cat = ctx.catalog()?;
    if a.dry_run {
        check_schedule_args(&a)?;
        let reg = json!({
            "mechanism": Mechanism::for_os(a.systemd),
            "time": a.time, "deep_day": a.deep_day, "deep_time": a.deep_time,
        });
        let (spec, mechanism) = schedule_spec_from_settings(ctx, &cat, &reg)?;
        if a.remove {
            let registered = registered_mechanism(&cat)?.unwrap_or(mechanism);
            if ctx.json {
                ctx.out_json(
                    "sb.setup/v1",
                    json!({"step": "schedule", "removed": true, "mechanism": registered}),
                );
            } else {
                eprintln!("Would remove the scheduled jobs ({registered:?}).");
            }
            return Ok(exit::OK);
        }
        let rendered = schedule::render_all(&spec, mechanism);
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
    let (report, message) = apply_schedule(ctx, &cat, &a)?;
    if ctx.json {
        ctx.out_json("sb.setup/v1", report);
    } else {
        eprintln!("{message}");
    }
    Ok(exit::OK)
}

/// Install (or remove) the skill for `targets` and record the installed set.
/// Returns one `{"target", "path"}` or `{"target", "removed"}` per change.
pub(crate) fn apply_skills(
    ctx: &Ctx,
    targets: &[Target],
    remove: bool,
) -> anyhow::Result<Vec<Value>> {
    let user_home = skills::user_home()?;
    let mut done = Vec::new();
    for t in targets {
        if remove {
            if skills::remove(*t, &user_home)? {
                done.push(json!({"target": t.name(), "removed": true}));
            }
        } else {
            let dir = skills::install(*t, &user_home)?;
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
    Ok(done)
}

/// A line describing one result of [`apply_skills`].
pub(crate) fn skills_message(d: &Value) -> String {
    let target = d["target"].as_str().unwrap_or("");
    match d.get("path") {
        Some(p) => format!(
            "Installed the skill for {target} at {}",
            p.as_str().unwrap_or("")
        ),
        None => format!("Removed the skill for {target}"),
    }
}

fn setup_skills(ctx: &Ctx, a: SetupSkillsArgs) -> anyhow::Result<i32> {
    let targets = Target::parse(&a.target)
        .ok_or_else(|| usage("--target is copilot, claude, codex or all"))?;
    let done = apply_skills(ctx, &targets, a.remove)?;
    if ctx.json {
        ctx.out_json(
            "sb.setup/v1",
            json!({"step": "skills", "version": skills::embedded_version(), "results": done}),
        );
    } else {
        for d in &done {
            eprintln!("{}", skills_message(d));
        }
    }
    Ok(exit::OK)
}

/// Persist `SECOND_BRAIN_HOME` for a non-default home without asking: the
/// Windows user environment, or the shell rc file. Returns a message.
pub(crate) fn persist_env(ctx: &Ctx) -> anyhow::Result<String> {
    let home = ctx.home.root();
    if cfg!(windows) {
        second_brain_setup::env::set_windows_user_env(home)?;
        return Ok("Set SECOND_BRAIN_HOME for your user. Open a new terminal to use it.".into());
    }
    let shell = std::env::var("SHELL").unwrap_or_default();
    let (rc, line) = second_brain_setup::env::shell_rc(&skills::user_home()?, &shell, home);
    second_brain_setup::env::write_rc(&rc, &line)?;
    Ok(format!(
        "Added to {}. Open a new shell to use it.",
        rc.display()
    ))
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
        let message = persist_env(ctx)?;
        if ctx.json {
            ctx.out_json(
                "sb.setup/v1",
                json!({"step": "env", "set": "HKCU\\Environment\\SECOND_BRAIN_HOME"}),
            );
        } else {
            eprintln!("{message}");
        }
        return Ok(exit::OK);
    }
    let shell = std::env::var("SHELL").unwrap_or_default();
    let (rc, line) = second_brain_setup::env::shell_rc(&skills::user_home()?, &shell, home);
    let write = yes
        || (util::interactive()
            && util::confirm(&format!("Add `{line}` to {}?", rc.display()), true)?);
    if write {
        second_brain_setup::env::write_rc(&rc, &line)?;
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

/// `sb setup --yes`: the steps that have defaults, without the setup screen.
fn defaults(ctx: &Ctx) -> anyhow::Result<i32> {
    eprintln!("== Home: {}", ctx.home.root().display());
    setup_home(ctx)?;
    if !ctx.home.is_default() {
        setup_env(ctx, true)?;
    }
    eprintln!("== Accounts: skipped. Use `sb account add google|slack`.");
    eprintln!("== Summarizer: skipped. Use `sb setup llm --preset ...`.");
    eprintln!("== Schedule");
    if let Err(e) = setup_schedule(
        ctx,
        SetupScheduleArgs {
            time: "19:30".into(),
            deep_day: "mon".into(),
            deep_time: "18:30".into(),
            systemd: false,
            remove: false,
            dry_run: false,
        },
    ) {
        eprintln!("error: {e:#}");
    }
    eprintln!("== Agent skills");
    let found: Vec<Target> = Target::ALL
        .into_iter()
        .filter(|t| t.is_installed_agent())
        .collect();
    for d in apply_skills(ctx, &found, false)? {
        eprintln!("{}", skills_message(&d));
    }
    let cat = ctx.catalog()?;
    let days: u64 = cat.setting_or("sync.initial_days", 30u64)?;
    eprintln!("== First sync: reaches back {days} days (sync.initial_days).");
    eprintln!(
        "To go further back later: `sb sync --since 90d` fetches only what is older than what is already synced."
    );
    eprintln!("\nSetup finished. `sb doctor` shows what still needs attention.");
    Ok(exit::OK)
}
