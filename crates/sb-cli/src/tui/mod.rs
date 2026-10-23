//! The terminal UI for `sb setup` and `sb config` (ADR-0020,
//! docs/specs/setup-and-scheduling.md).
//!
//! `app.rs` holds the state and key handling, `view.rs` draws it. This module
//! owns the terminal: it runs the event loop, and leaves the screen for jobs
//! that print or prompt (Google consent, `sb sync`).

mod app;
mod form;
mod view;

#[cfg(test)]
mod tests;

use std::io::{BufRead, IsTerminal, Stdout, Write};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use tokio::runtime::Handle;

use crate::Ctx;
use crate::cli::{AccountAdd, AccountCmd, SyncArgs};
use crate::commands::{admin, ingest, setup};
use crate::util::{exit, usage};
use app::{App, Job, Outcome};

/// Where the screen opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Start {
    /// `sb setup`: the first section; the home is created first.
    Setup,
    /// `sb config`, `sb config edit`: the settings section.
    Settings,
    /// `sb config edit --account <id>`: that account's config.
    Account(String),
}

/// Open the screen and run it until the user quits. Returns the exit code.
pub(crate) async fn run(ctx: &Ctx, start: Start) -> anyhow::Result<i32> {
    let what = match start {
        Start::Setup => {
            "the setup screen needs a terminal; use `sb setup --yes` or the steps \
             (`sb setup home`, `sb setup llm --preset ...`, ...)"
        }
        _ => {
            "the settings screen needs a terminal; use `sb config list|get|set|unset` \
             or `sb config edit --editor`"
        }
    };
    if ctx.json || !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(usage(what));
    }
    if start == Start::Setup {
        second_brain_setup::home::setup_home(&ctx.home)?;
    }
    let mut app = App::new(ctx, start)?;
    tokio::task::block_in_place(|| event_loop(ctx, &mut app))
}

/// The alternate screen in raw mode. Dropping it restores the terminal.
struct Screen {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
}

impl Screen {
    fn enter() -> anyhow::Result<Self> {
        enable_raw_mode()?;
        let entered = execute!(std::io::stdout(), EnterAlternateScreen)
            .map_err(anyhow::Error::from)
            .and_then(|_| Ok(Terminal::new(CrosstermBackend::new(std::io::stdout()))?));
        match entered {
            Ok(terminal) => Ok(Screen { terminal }),
            Err(e) => {
                restore();
                Err(e)
            }
        }
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        restore();
        let _ = self.terminal.show_cursor();
    }
}

fn event_loop(ctx: &Ctx, app: &mut App) -> anyhow::Result<i32> {
    let handle = Handle::current();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
    let mut screen = Screen::enter()?;
    loop {
        screen.terminal.draw(|f| view::render(app, f))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        // Windows reports releases too.
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match app.on_key(ctx, key) {
            Outcome::None => {}
            Outcome::Quit => break,
            Outcome::Job(job) if job.suspends() => {
                drop(screen);
                eprintln!("\n== {}\n", job.title());
                let sync = matches!(job, Job::Sync);
                let r = handle.block_on(run_job(ctx, app, job));
                if sync {
                    return r.map(|(code, _)| code);
                }
                if ctx.cancel.is_cancelled() {
                    return Ok(exit::INTERRUPTED);
                }
                let message = match &r {
                    Ok((_, m)) => m.clone(),
                    Err(e) => {
                        eprintln!("error: {e:#}");
                        format!("{e:#}")
                    }
                };
                pause();
                screen = Screen::enter()?;
                app.finished(match r {
                    Ok(_) => Ok(message),
                    Err(e) => Err(e),
                });
            }
            Outcome::Job(job) => {
                app.status = Some(app::Status {
                    text: job.title(),
                    error: false,
                });
                screen.terminal.draw(|f| view::render(app, f))?;
                let r = handle.block_on(run_job(ctx, app, job));
                app.finished(r.map(|(_, m)| m));
            }
        }
    }
    Ok(exit::OK)
}

/// Wait for Enter before going back to the screen.
fn pause() {
    eprint!("\nPress Enter to return to the screen.");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
}

/// Run a job. Returns its exit code and a one-line summary.
async fn run_job(ctx: &Ctx, app: &App, job: Job) -> anyhow::Result<(i32, String)> {
    match job {
        Job::AddGoogle {
            id,
            client_secret,
            features,
            label,
        } => {
            let code = admin::account(
                ctx,
                AccountCmd::Add(AccountAdd::Google {
                    id: id.clone(),
                    client_secret,
                    label,
                    features,
                    no_browser: false,
                }),
            )
            .await?;
            Ok((code, format!("Added Google account {id}.")))
        }
        Job::AddSlack { id, token, label } => {
            let code = admin::account(
                ctx,
                AccountCmd::Add(AccountAdd::Slack {
                    id: id.clone(),
                    token: Some(token.expose().to_string()),
                    label,
                }),
            )
            .await?;
            Ok((code, format!("Added Slack account {id}.")))
        }
        Job::Llm {
            preset,
            name,
            profile,
            make_default,
            test,
        } => {
            let r =
                setup::save_llm(ctx, &app.cat, &name, &preset, profile, make_default, test).await?;
            let mut parts = vec![format!(
                "Saved {}{}.",
                r.name,
                if make_default { " as the default" } else { "" }
            )];
            parts.extend(setup::test_message(&r.test));
            parts.extend(r.warnings.iter().map(|w| format!("warning: {w}")));
            let text = parts.join(" ");
            if r.test.get("ok").and_then(serde_json::Value::as_bool) == Some(false) {
                return Err(crate::util::failure("llm.test", text));
            }
            Ok((exit::OK, text))
        }
        Job::Estimate | Job::Sync => {
            let estimate = matches!(job, Job::Estimate);
            let code = ingest::sync(
                ctx,
                SyncArgs {
                    accounts: vec![],
                    sources: vec![],
                    deep: false,
                    since: None,
                    until: None,
                    no_summary: false,
                    limits: Default::default(),
                    dry_run: false,
                    estimate,
                },
            )
            .await?;
            Ok((code, "Estimated the first sync.".into()))
        }
    }
}
