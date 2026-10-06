//! Command-line definitions (docs/specs/cli.md).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "second-brain",
    version,
    about = "A personal knowledge base for AI agents",
    long_about = "second-brain collects Slack conversations and Google Meet notes, stores them \
locally with summaries, and makes them searchable. `sb` is a short alias."
)]
pub struct Cli {
    /// Override SECOND_BRAIN_HOME.
    #[arg(long, global = true, value_name = "DIR")]
    pub home: Option<PathBuf>,
    /// Machine-readable output.
    #[arg(long, global = true)]
    pub json: bool,
    /// More log output (repeatable).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// Less output.
    #[arg(short, long, global = true)]
    pub quiet: bool,
    #[arg(long, global = true)]
    pub no_color: bool,
    /// What started the run (set by scheduled jobs).
    #[arg(long, global = true, hide = true, value_name = "TRIGGER")]
    pub trigger: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Interactive setup wizard, or one setup step.
    Setup(SetupCmd),
    /// Global settings and secrets.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Manage accounts.
    #[command(subcommand)]
    Account(AccountCmd),
    /// Credential status and re-authentication.
    #[command(subcommand)]
    Auth(AuthCmd),
    /// Incremental, resumable sync of all enabled accounts, then summaries.
    Sync(SyncArgs),
    /// Fetch raw data again by natural key (replaces all segments).
    Refetch(RefetchArgs),
    /// Re-run normalization on stored raw data (no network).
    Reextract(ReextractArgs),
    /// Summarize pending entries.
    Summarize(SummarizeArgs),
    /// Overwrite generated sections of matching entries.
    Resummarize(ResummarizeArgs),
    /// Add Google Docs, web pages or local files (single-item ingest).
    Ingest(IngestArgs),
    /// Import a second-brain-import/v1 bundle.
    Import(ImportArgs),
    /// Search entries.
    Search(SearchArgs),
    /// Print an entry.
    Show(ShowArgs),
    /// List entries, newest first.
    List(ListArgs),
    /// Counts by account, source, raw status and summary status/model.
    Stats,
    /// Summarization spend against the weekly and monthly budget.
    Budget(BudgetArgs),
    /// Show generated sections next to the source text.
    Review(ReviewArgs),
    /// Health checks and open issues.
    Doctor(DoctorArgs),
    /// Search index maintenance.
    #[command(subcommand)]
    Index(IndexCmd),
    /// Version, build target and skill version.
    Version,
}

// ---------- setup ----------

#[derive(Debug, Args)]
pub struct SetupCmd {
    /// Accept defaults (non-interactive).
    #[arg(long, global = true)]
    pub yes: bool,
    #[command(subcommand)]
    pub step: Option<SetupStep>,
}

#[derive(Debug, Subcommand)]
pub enum SetupStep {
    /// Create the home directory, database, permissions and pseudo-accounts.
    Home,
    /// Create or edit a summarizer profile and test it.
    Llm(SetupLlmArgs),
    /// Register or remove scheduled jobs.
    Schedule(SetupScheduleArgs),
    /// Install agent skill files.
    Skills(SetupSkillsArgs),
    /// Persist SECOND_BRAIN_HOME when a non-default home is used.
    Env,
}

#[derive(Debug, Args)]
pub struct SetupLlmArgs {
    /// Preset: anthropic, openai, claude_cli, copilot_cli, codex_cli, antigravity_cli, local.
    #[arg(long)]
    pub preset: Option<String>,
    /// Profile name (default: the preset key).
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub base_url: Option<String>,
    /// Do not make it the default profile.
    #[arg(long)]
    pub no_default: bool,
    /// Skip the test call.
    #[arg(long)]
    pub no_test: bool,
}

#[derive(Debug, Args)]
pub struct SetupScheduleArgs {
    /// Daily sync time.
    #[arg(long, default_value = "19:30", value_name = "HH:MM")]
    pub time: String,
    /// Day of the weekly deep sync.
    #[arg(long, default_value = "mon", value_name = "DAY")]
    pub deep_day: String,
    #[arg(long, default_value = "18:30", value_name = "HH:MM")]
    pub deep_time: String,
    /// Linux: use systemd user timers instead of crontab.
    #[arg(long)]
    pub systemd: bool,
    /// Remove the scheduled jobs.
    #[arg(long)]
    pub remove: bool,
    /// Print what would be registered without installing.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct SetupSkillsArgs {
    /// copilot, claude, codex or all.
    #[arg(long, default_value = "all")]
    pub target: String,
    #[arg(long)]
    pub remove: bool,
}

// ---------- config ----------

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// List settings (secrets masked).
    List,
    Get {
        key: String,
    },
    /// Set a setting. The value is parsed as JSON, or taken as a string.
    Set {
        key: String,
        value: String,
    },
    Unset {
        key: String,
    },
    /// Store a secret read from a hidden prompt or stdin.
    SetSecret {
        name: String,
        #[arg(long)]
        account: Option<String>,
    },
    /// Edit settings or an account's config as TOML in $EDITOR.
    Edit {
        #[arg(long)]
        account: Option<String>,
    },
}

// ---------- account / auth ----------

#[derive(Debug, Subcommand)]
pub enum AccountCmd {
    /// Add an account.
    #[command(subcommand)]
    Add(AccountAdd),
    List,
    Show {
        id: String,
    },
    Disable {
        id: String,
    },
    Enable {
        id: String,
    },
    /// Remove an account. `--purge` also deletes its entries and raw files.
    Remove {
        id: String,
        #[arg(long)]
        purge: bool,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum AccountAdd {
    /// Import an OAuth client and run the Google consent flow.
    Google {
        id: String,
        /// Client JSON file, or `global` to reuse the stored global client.
        #[arg(long, value_name = "FILE")]
        client_secret: String,
        #[arg(long)]
        label: Option<String>,
        /// Comma-separated features: meet, docs, gmail.
        #[arg(long, default_value = "meet", value_delimiter = ',')]
        features: Vec<String>,
        /// Print the consent URL instead of opening a browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// Validate a Slack user token (xoxp-) and store it.
    Slack {
        id: String,
        /// Token; prefer the hidden prompt so it stays out of shell history.
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        label: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum AuthCmd {
    /// Validity of every account's credentials.
    Status {
        /// Also check credentials live (token refresh, auth.test).
        #[arg(long)]
        online: bool,
    },
    /// Re-authenticate an account.
    Login {
        id: String,
        #[arg(long)]
        allow_identity_change: bool,
        #[arg(long)]
        no_browser: bool,
    },
}

// ---------- ingestion ----------

/// Common entry filters.
#[derive(Debug, Args, Default, Clone)]
pub struct Filters {
    #[arg(long = "account", value_name = "ID")]
    pub accounts: Vec<String>,
    #[arg(long = "source", value_name = "KIND")]
    pub sources: Vec<String>,
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,
    #[arg(long, value_name = "DATE")]
    pub until: Option<String>,
    #[arg(long = "entry", value_name = "UID")]
    pub entries: Vec<String>,
    #[arg(long, value_name = "STATUS")]
    pub raw_status: Vec<String>,
    #[arg(long, value_name = "STATUS")]
    pub summary_status: Vec<String>,
}

/// Run limits.
#[derive(Debug, Args, Default, Clone)]
pub struct LimitArgs {
    #[arg(long, value_name = "N")]
    pub max_summaries: Option<u64>,
    #[arg(long, value_name = "USD")]
    pub max_cost: Option<f64>,
    /// e.g. 30m, 2h, 1h30m.
    #[arg(long, value_name = "DURATION")]
    pub time_limit: Option<String>,
}

#[derive(Debug, Args)]
pub struct SyncArgs {
    #[arg(long = "account", value_name = "ID")]
    pub accounts: Vec<String>,
    #[arg(long = "source", value_name = "KIND")]
    pub sources: Vec<String>,
    /// Also dormant conversations and all watched threads.
    #[arg(long)]
    pub deep: bool,
    /// Extend the data backwards to this date or age (`2026-07-01`, `90d`, `12w`).
    #[arg(long, value_name = "DATE|AGE")]
    pub since: Option<String>,
    /// With --since: fetch the explicit window [since, until) regardless of coverage.
    #[arg(long, value_name = "DATE|AGE")]
    pub until: Option<String>,
    #[arg(long)]
    pub no_summary: bool,
    #[command(flatten)]
    pub limits: LimitArgs,
    /// Show what would run, without network access.
    #[arg(long)]
    pub dry_run: bool,
    /// Estimate pending summarization tokens and cost.
    #[arg(long)]
    pub estimate: bool,
}

#[derive(Debug, Args)]
pub struct RefetchArgs {
    #[command(flatten)]
    pub filters: Filters,
    /// Only entries whose raw data is missing or failed.
    #[arg(long)]
    pub raw_missing: bool,
    #[arg(long, value_name = "DURATION")]
    pub time_limit: Option<String>,
}

#[derive(Debug, Args)]
pub struct ReextractArgs {
    #[command(flatten)]
    pub filters: Filters,
    #[arg(long, value_name = "DURATION")]
    pub time_limit: Option<String>,
}

#[derive(Debug, Args)]
pub struct SummarizeArgs {
    #[command(flatten)]
    pub filters: Filters,
    #[command(flatten)]
    pub limits: LimitArgs,
    /// Reset attempt counters of failed entries first.
    #[arg(long)]
    pub retry_failed: bool,
    /// Use this profile instead of the configured ones.
    #[arg(long)]
    pub profile: Option<String>,
    #[arg(long)]
    pub estimate: bool,
}

#[derive(Debug, Args)]
pub struct ResummarizeArgs {
    #[command(flatten)]
    pub filters: Filters,
    #[arg(long, conflicts_with = "native")]
    pub profile: Option<String>,
    /// Restore source-native summaries (Gemini notes) from raw data.
    #[arg(long)]
    pub native: bool,
    #[arg(long, value_name = "MODEL")]
    pub where_model: Option<String>,
    #[arg(long, value_name = "PROVIDER")]
    pub where_provider: Option<String>,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub estimate: bool,
    #[arg(long, value_name = "N")]
    pub limit: Option<u64>,
    #[arg(long, value_name = "USD")]
    pub max_cost: Option<f64>,
    #[arg(long, value_name = "DURATION")]
    pub time_limit: Option<String>,
    /// Redo entries already at the target generator.
    #[arg(long)]
    pub force: bool,
    /// Do not ask for confirmation.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct IngestArgs {
    /// URLs or file paths (Google Docs/Drive links, web pages, local files).
    #[arg(required = true, value_name = "URL_OR_PATH")]
    pub locators: Vec<String>,
    /// The Google account that reads Google URLs (default: the first one that can).
    #[arg(long, value_name = "ID")]
    pub account: Option<String>,
    /// Replace the extracted title (one locator only).
    #[arg(long)]
    pub title: Option<String>,
    /// Why this matters; stored as the entry's background section.
    #[arg(long)]
    pub context: Option<String>,
    /// Replace the document date (one locator only).
    #[arg(long, value_name = "DATE")]
    pub date: Option<String>,
    /// Fetch again even if nothing changed; also adds duplicate local files.
    #[arg(long)]
    pub force: bool,
    /// Also store the original file or page (default: only the extracted text).
    #[arg(long)]
    pub keep_original: bool,
    /// Store the entry without summarizing it now.
    #[arg(long)]
    pub no_summary: bool,
    /// Show what would happen; no network access and no writes.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    pub bundle: PathBuf,
    /// Account mapping override, e.g. slack=acme-slack.
    #[arg(long = "map", value_name = "KIND=ACCOUNT")]
    pub maps: Vec<String>,
    #[arg(long)]
    pub dry_run: bool,
}

// ---------- retrieval ----------

#[derive(Debug, Args)]
pub struct SearchArgs {
    #[arg(required = true)]
    pub terms: Vec<String>,
    #[arg(long = "section", value_name = "KIND")]
    pub sections: Vec<String>,
    #[arg(long = "source", value_name = "KIND")]
    pub sources: Vec<String>,
    #[arg(long = "account", value_name = "ID")]
    pub accounts: Vec<String>,
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,
    #[arg(long, value_name = "DATE")]
    pub until: Option<String>,
    #[arg(long, default_value_t = 8)]
    pub limit: u32,
    /// Return every matching section instead of the best one per entry.
    #[arg(long)]
    pub all_sections: bool,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    pub entry_uid: String,
    #[arg(long = "section", value_name = "KIND")]
    pub sections: Vec<String>,
    /// Print raw file paths; with --role, the content.
    #[arg(long)]
    pub raw: bool,
    #[arg(long, requires = "raw")]
    pub role: Option<String>,
    /// Include metadata.
    #[arg(long)]
    pub meta: bool,
}

#[derive(Debug, Args)]
pub struct BudgetArgs {
    /// Weeks of history to show.
    #[arg(long, default_value_t = 8, value_name = "N")]
    pub weeks: u32,
    /// Months of history to show.
    #[arg(long, default_value_t = 6, value_name = "N")]
    pub months: u32,
    /// Also show the spend per model.
    #[arg(long)]
    pub by_model: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub filters: Filters,
    #[arg(long, default_value_t = 20)]
    pub limit: u64,
}

#[derive(Debug, Args)]
pub struct ReviewArgs {
    #[command(flatten)]
    pub filters: Filters,
    /// Slack channel name or ID.
    #[arg(long)]
    pub channel: Option<String>,
    #[arg(long, default_value_t = 5)]
    pub limit: u64,
    /// Source text lines shown per entry.
    #[arg(long, default_value_t = 20)]
    pub detail_lines: usize,
    /// Show the whole source text.
    #[arg(long)]
    pub full: bool,
    /// Include entries without summaries.
    #[arg(long)]
    pub all: bool,
}

// ---------- operations ----------

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Also run checks that need the network.
    #[arg(long)]
    pub online: bool,
    /// Fix what can be fixed.
    #[arg(long)]
    pub fix: bool,
}

#[derive(Debug, Subcommand)]
pub enum IndexCmd {
    /// Rebuild the active search backend(s).
    Rebuild,
}
