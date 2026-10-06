//! The `second-brain` / `sb` command-line interface.

mod cli;
mod commands;
mod factory;
mod util;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use sb_core::{Language, RunTrigger};
use sb_pipeline::{Pipeline, PipelineError, Progress};
use sb_store::{Catalog, Home, StoreError};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use cli::{Cli, Command};
use util::{CliError, exit};

/// Shared command context.
pub(crate) struct Ctx {
    pub home: Home,
    pub json: bool,
    pub quiet: bool,
    pub trigger: RunTrigger,
    pub cancel: CancellationToken,
}

impl Ctx {
    /// Open the catalog, with a helpful error when the home is not set up.
    pub fn catalog(&self) -> anyhow::Result<Catalog> {
        match Catalog::open(&self.home) {
            Ok(c) => Ok(c),
            Err(StoreError::NotInitialized(p)) => Err(CliError {
                code: "home.not_initialized",
                exit: exit::FAILURE,
                message: format!(
                    "{} is not set up; run `sb setup` (or `sb setup home`)",
                    p.display()
                ),
            }
            .into()),
            Err(e) => Err(e.into()),
        }
    }

    /// The display language (`display.language`).
    pub fn language(&self, cat: &Catalog) -> Language {
        cat.setting_or("display.language", "en".to_string())
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(Language::En)
    }

    /// A pipeline over a fresh catalog connection, with progress on stderr.
    pub fn pipeline(&self) -> anyhow::Result<Pipeline> {
        let cat = self.catalog()?;
        let mut p = Pipeline::new(cat, Arc::new(factory::Factory)).with_trigger(self.trigger);
        p.cancel = self.cancel.clone();
        if !self.quiet && !self.json {
            p = p.with_progress(Arc::new(|ev: Progress| match ev {
                Progress::Stage(s) => eprintln!("==> {s}"),
                Progress::Fetched {
                    account,
                    committed,
                    queued,
                } => {
                    eprintln!("    {account}: {committed} committed, {queued} queued")
                }
                Progress::Summarized {
                    done,
                    remaining,
                    cost_usd,
                } => {
                    eprintln!("    summaries: {done} done, {remaining} remaining, ~${cost_usd:.4}")
                }
                Progress::Warning(w) => eprintln!("warning: {w}"),
            }));
        }
        Ok(p)
    }

    /// Print `schema`-tagged JSON.
    pub fn out_json(&self, schema: &str, mut v: serde_json::Value) {
        if let Some(o) = v.as_object_mut() {
            o.insert("schema".into(), json!(schema));
        }
        util::print_json(&v);
    }
}

fn init_logging(cli: &Cli, home: &Home, scheduled: bool) {
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{EnvFilter, fmt};
    let default = if cli.quiet {
        "error"
    } else {
        match cli.verbose {
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        }
    };
    let filter = EnvFilter::try_from_env("SB_LOG").unwrap_or_else(|_| EnvFilter::new(default));
    let stderr = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(!cli.no_color && std::env::var_os("NO_COLOR").is_none())
        .with_target(false)
        .with_filter(filter);
    let file_layer = if scheduled {
        scheduled_log_file(home)
    } else {
        None
    }
    .map(|f| {
        fmt::layer()
            .with_writer(std::sync::Mutex::new(f))
            .with_ansi(false)
            .with_filter(
                EnvFilter::try_from_env("SB_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
            )
    });
    let _ = tracing_subscriber::registry()
        .with(stderr)
        .with(file_layer)
        .try_init();
}

/// `logs/<date>.log`, keeping 30 days.
fn scheduled_log_file(home: &Home) -> Option<std::fs::File> {
    let dir = home.logs_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 86_400);
    for e in std::fs::read_dir(&dir).ok()?.flatten() {
        let p: PathBuf = e.path();
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t < cutoff);
        if old && p.extension().is_some_and(|x| x == "log") {
            let _ = std::fs::remove_file(p);
        }
    }
    let name = format!("{}.log", chrono::Local::now().format("%Y-%m-%d"));
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(name))
        .ok()
}

/// First signal: graceful stop. Second signal: abort immediately.
fn install_signal_handlers(cancel: CancellationToken) {
    tokio::spawn(async move {
        loop {
            wait_for_signal().await;
            if cancel.is_cancelled() {
                eprintln!("\nAborting.");
                std::process::exit(exit::INTERRUPTED);
            }
            eprintln!("\nStopping: finishing in-flight work (press Ctrl+C again to abort)...");
            cancel.cancel();
        }
    });
}

#[cfg(unix)]
async fn wait_for_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

#[cfg(windows)]
async fn wait_for_signal() {
    use tokio::signal::windows;
    let (Ok(mut brk), Ok(mut close)) = (windows::ctrl_break(), windows::ctrl_close()) else {
        let _ = tokio::signal::ctrl_c().await;
        return;
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = brk.recv() => {}
        _ = close.recv() => {}
    }
}

#[cfg(not(any(unix, windows)))]
async fn wait_for_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

fn error_exit(err: &anyhow::Error, json: bool) -> i32 {
    let (code, status) = if let Some(c) = err.downcast_ref::<CliError>() {
        (c.code, c.exit)
    } else if let Some(p) = err.downcast_ref::<PipelineError>() {
        match p {
            PipelineError::Locked => ("sync.locked", exit::LOCKED),
            PipelineError::Invalid(_) => ("invalid", exit::USAGE),
            _ => ("failed", exit::FAILURE),
        }
    } else {
        ("failed", exit::FAILURE)
    };
    let message = if code == "sync.locked" {
        "another sync is running; try again later".to_string()
    } else {
        format!("{err:#}")
    };
    if json {
        util::print_json(
            &json!({"schema": "sb.error/v1", "error": {"code": code, "message": message}}),
        );
    } else {
        eprintln!("error: {message}");
    }
    status
}

/// Entry point shared by both binaries. Returns the exit code.
pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let json_requested = args.iter().any(|a| a == "--json");
    let cli = match Cli::try_parse_from(&args) {
        Ok(c) => c,
        Err(e) => {
            use clap::error::ErrorKind;
            if matches!(
                e.kind(),
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) {
                let _ = e.print();
                return exit::OK;
            }
            if json_requested {
                util::print_json(
                    &json!({"schema": "sb.error/v1", "error": {"code": "usage", "message": e.to_string().trim()}}),
                );
            } else {
                let _ = e.print();
            }
            return exit::USAGE;
        }
    };
    let home = match Home::resolve(cli.home.as_deref()) {
        Ok(h) => h,
        Err(e) => return error_exit(&e.into(), cli.json),
    };
    let trigger = match cli
        .trigger
        .clone()
        .or_else(|| std::env::var("SB_TRIGGER").ok())
        .as_deref()
    {
        Some("schedule") => RunTrigger::Schedule,
        Some("mcp") => RunTrigger::Mcp,
        _ => RunTrigger::Manual,
    };
    init_logging(&cli, &home, trigger == RunTrigger::Schedule);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => return error_exit(&e.into(), cli.json),
    };
    let json_out = cli.json;
    runtime.block_on(async move {
        let cancel = CancellationToken::new();
        install_signal_handlers(cancel.clone());
        let ctx = Ctx {
            home,
            json: cli.json,
            quiet: cli.quiet,
            trigger,
            cancel,
        };
        match run(&ctx, cli.command).await {
            Ok(code) => code,
            Err(e) => error_exit(&e, json_out),
        }
    })
}

async fn run(ctx: &Ctx, cmd: Command) -> anyhow::Result<i32> {
    use commands::*;
    match cmd {
        Command::Setup(s) => setup::run(ctx, s).await,
        Command::Config(c) => admin::config(ctx, c),
        Command::Account(a) => admin::account(ctx, a).await,
        Command::Auth(a) => admin::auth(ctx, a).await,
        Command::Sync(a) => ingest::sync(ctx, a).await,
        Command::Refetch(a) => ingest::refetch(ctx, a).await,
        Command::Reextract(a) => ingest::reextract(ctx, a).await,
        Command::Summarize(a) => ingest::summarize(ctx, a).await,
        Command::Resummarize(a) => ingest::resummarize(ctx, a).await,
        Command::Ingest(a) => ingest::ingest(ctx, a).await,
        Command::Import(a) => ingest::import(ctx, a),
        Command::Search(a) => retrieval::search(ctx, a),
        Command::Show(a) => retrieval::show(ctx, a),
        Command::List(a) => retrieval::list(ctx, a),
        Command::Stats => retrieval::stats(ctx),
        Command::Budget(a) => budget::run(ctx, a),
        Command::Review(a) => retrieval::review(ctx, a),
        Command::Doctor(a) => doctor::run(ctx, a).await,
        Command::Index(cli::IndexCmd::Rebuild) => admin::index_rebuild(ctx),
        Command::Version => admin::version(ctx),
    }
}
