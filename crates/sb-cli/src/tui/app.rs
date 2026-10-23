//! State and key handling of the setup and settings screen. Rendering is in
//! `view.rs`; work that needs the terminal or the network is returned as a
//! [`Job`] for the event loop in `mod.rs`.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use second_brain_kernel::{AccountKind, AccountStatus, Secret};
use second_brain_setup::llm::Preset;
use second_brain_setup::schedule::{TimeOfDay, Weekday};
use second_brain_setup::skills::{self, Target};
use second_brain_store::secrets::SecretInfo;
use second_brain_store::settings::{SettingItem, default_value};
use second_brain_store::{Account, Catalog, SecretScope};
use serde_json::{Value, json};

use super::Start;
use super::form::{Field, Form, FormEvent};
use crate::Ctx;
use crate::cli::SetupScheduleArgs;
use crate::commands::{admin, setup};

/// The sections in the navigation column, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Home,
    Accounts,
    Summarizer,
    Schedule,
    Skills,
    FirstSync,
    Settings,
    Secrets,
}

impl Section {
    pub const ALL: [Section; 8] = [
        Section::Home,
        Section::Accounts,
        Section::Summarizer,
        Section::Schedule,
        Section::Skills,
        Section::FirstSync,
        Section::Settings,
        Section::Secrets,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Home => "Home",
            Section::Accounts => "Accounts",
            Section::Summarizer => "Summarizer",
            Section::Schedule => "Schedule",
            Section::Skills => "Agent skills",
            Section::FirstSync => "First sync",
            Section::Settings => "Settings",
            Section::Secrets => "Secrets",
        }
    }

    /// Explanatory lines shown above the rows.
    pub fn intro(self) -> &'static [&'static str] {
        match self {
            Section::Home => &["The home directory holds the database, raw files and logs."],
            Section::Accounts => &[
                "Google: needs an OAuth client of type \"Desktop app\" (see the README guide).",
                "Slack: create an app from assets/slack-app-manifest.yaml, install it,",
                "and copy the User OAuth Token (xoxp-...).",
            ],
            Section::Summarizer => &[
                "The summarizer writes summaries of Slack threads and re-summarizes meetings.",
                "Enter makes a profile the default.",
            ],
            Section::Schedule => &[
                "Scheduled jobs run `sb sync` every day and a deep sync once a week.",
                "Change the times, then register the jobs.",
            ],
            Section::Skills => &[
                "The skill teaches AI agents to search second-brain. Install it for the",
                "agents you use; install again after updating sb.",
            ],
            Section::FirstSync => &[
                "The first sync reaches back this many days for every source.",
                "Later, `sb sync --since 90d` fetches only what is older than what is synced.",
            ],
            Section::Settings => &[
                "Global settings (`sb config list`). Grey values are defaults.",
                "Enter edits; x resets to the default.",
            ],
            Section::Secrets => &["Stored credentials, masked. Enter replaces a value."],
        }
    }
}

/// Fields of the schedule draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchedField {
    Time,
    DeepDay,
    DeepTime,
}

/// What a row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Act {
    Info(String),
    PersistEnv,
    EditAccount(String),
    ToggleAccount(String, bool),
    AddGoogle,
    AddSlack,
    MakeDefault(String),
    RemoveProfile(String),
    AddProfile,
    EditSchedule(SchedField),
    ToggleSystemd,
    RegisterSchedule,
    RemoveSchedule,
    InstallSkill(Target),
    RemoveSkill(Target),
    EditInitialDays,
    Estimate,
    RunSync,
    EditSetting(String),
    ResetSetting(String),
    AddSetting,
    SetSecret(String, String),
    RemoveSecret(String, String),
    AddSecret,
}

impl Act {
    /// Short label for the key help line.
    pub fn label(&self) -> &'static str {
        match self {
            Act::Info(_) => "details",
            Act::PersistEnv => "persist",
            Act::EditAccount(_) => "edit config",
            Act::ToggleAccount(_, true) => "disable",
            Act::ToggleAccount(_, false) => "enable",
            Act::AddGoogle | Act::AddSlack | Act::AddProfile | Act::AddSetting | Act::AddSecret => {
                "add"
            }
            Act::MakeDefault(_) => "make default",
            Act::RemoveProfile(_) | Act::RemoveSkill(_) | Act::RemoveSecret(..) => "remove",
            Act::EditSchedule(_) | Act::EditInitialDays | Act::EditSetting(_) => "edit",
            Act::ToggleSystemd => "toggle",
            Act::RegisterSchedule => "register",
            Act::RemoveSchedule => "remove",
            Act::InstallSkill(_) => "install",
            Act::Estimate => "estimate",
            Act::RunSync => "run",
            Act::ResetSetting(_) => "reset",
            Act::SetSecret(..) => "replace",
        }
    }
}

/// How a row is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Normal,
    /// Defaults and read-only values.
    Dim,
    /// Rows that start something (`+ Add ...`).
    Action,
    Warn,
}

#[derive(Debug, Clone)]
pub(crate) struct Row {
    pub text: String,
    pub tone: Tone,
    /// Enter.
    pub primary: Option<Act>,
    /// `x` / Delete.
    pub secondary: Option<Act>,
}

impl Row {
    fn new(text: impl Into<String>, tone: Tone, primary: Option<Act>) -> Self {
        Row {
            text: text.into(),
            tone,
            primary,
            secondary: None,
        }
    }

    fn alt(mut self, act: Act) -> Self {
        self.secondary = Some(act);
        self
    }
}

/// The schedule as it will be registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScheduleDraft {
    pub time: String,
    pub deep_day: String,
    pub deep_time: String,
    pub systemd: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct SkillRow {
    pub target: Target,
    pub agent_found: bool,
    pub installed: Option<String>,
}

/// Catalog data shown on the screen, reloaded after every change.
#[derive(Debug, Clone, Default)]
pub(crate) struct Snapshot {
    pub accounts: Vec<Account>,
    pub profiles: Vec<(String, Value)>,
    pub default_profile: Option<String>,
    pub schedule: Option<Value>,
    pub skills: Vec<SkillRow>,
    pub skill_version: String,
    pub initial_days: u64,
    pub settings: Vec<SettingItem>,
    pub secrets: Vec<SecretInfo>,
}

/// One key of an account's config.
#[derive(Debug, Clone)]
pub(crate) struct ConfigRow {
    pub key: String,
    pub value: Value,
    /// Not stored; the source's default applies.
    pub default: bool,
}

/// The editor for one account's `config`.
#[derive(Debug, Clone)]
pub(crate) struct AccountEditor {
    pub id: String,
    pub title: String,
    pub rows: Vec<ConfigRow>,
    pub cursor: usize,
}

/// Keys of an account config shown but not edited here.
const READ_ONLY_ACCOUNT_KEYS: &[&str] = &["scopes"];

/// Settings that `sb` maintains itself.
const READ_ONLY_SETTINGS: &[&str] = &["backup.last_at", "skills.installed", "schedule.registered"];

impl AccountEditor {
    fn load(cat: &Catalog, id: &str) -> anyhow::Result<Self> {
        let a = cat
            .account_by_str(id)?
            .ok_or_else(|| crate::util::failure("account.not_found", format!("no account {id}")))?;
        let defaults = match a.kind {
            AccountKind::Slack => second_brain_slack::default_config_json(),
            AccountKind::Google => {
                let features: Vec<String> = a
                    .config
                    .get("features")
                    .and_then(|f| serde_json::from_value(f.clone()).ok())
                    .unwrap_or_default();
                second_brain_google::default_config_json(&features)
            }
            _ => json!({}),
        };
        let mut rows = Vec::new();
        if let Some(d) = defaults.as_object() {
            for (k, v) in d {
                let stored = a.config.get(k);
                rows.push(ConfigRow {
                    key: k.clone(),
                    value: stored.cloned().unwrap_or_else(|| v.clone()),
                    default: stored.is_none(),
                });
            }
        }
        if let Some(c) = a.config.as_object() {
            let mut extra: Vec<_> = c
                .iter()
                .filter(|(k, _)| !rows.iter().any(|r| r.key == **k))
                .collect();
            extra.sort_by(|x, y| x.0.cmp(y.0));
            for (k, v) in extra {
                rows.push(ConfigRow {
                    key: k.clone(),
                    value: v.clone(),
                    default: false,
                });
            }
        }
        Ok(AccountEditor {
            id: a.id.to_string(),
            title: format!("{} ({}, {})", a.id, a.kind, a.label),
            rows,
            cursor: 0,
        })
    }
}

/// What a submitted form does.
#[derive(Debug, Clone)]
pub(crate) enum Purpose {
    Setting(String),
    NewSetting,
    Secret(String, String),
    NewSecret,
    Google,
    Slack,
    Profile(Box<Preset>),
    Schedule(SchedField),
    InitialDays,
    AccountValue(String, String),
}

/// What a picked option does.
#[derive(Debug, Clone)]
pub(crate) enum PickPurpose {
    Preset,
    Setting(String),
    AccountValue(String, String),
}

/// Overlays, topmost last.
#[derive(Debug, Clone)]
pub(crate) enum Modal {
    Form(Box<Form>, Purpose),
    Pick {
        title: String,
        options: Vec<String>,
        cursor: usize,
        purpose: PickPurpose,
    },
    /// A yes/no question; "yes" runs the action.
    Confirm {
        text: String,
        then: Act,
    },
    Account(AccountEditor),
}

/// Work done by the event loop, outside of key handling.
pub(crate) enum Job {
    AddGoogle {
        id: String,
        client_secret: String,
        features: Vec<String>,
        label: Option<String>,
    },
    AddSlack {
        id: String,
        token: Secret,
        label: Option<String>,
    },
    Llm {
        preset: Box<Preset>,
        name: String,
        profile: Value,
        make_default: bool,
        test: bool,
    },
    Estimate,
    Sync,
}

impl Job {
    /// Whether the job leaves the screen and runs in the plain terminal (it
    /// prints, prompts or opens a browser).
    pub fn suspends(&self) -> bool {
        !matches!(self, Job::Llm { .. })
    }

    pub fn title(&self) -> String {
        match self {
            Job::AddGoogle { id, .. } => format!("Adding Google account {id}"),
            Job::AddSlack { id, .. } => format!("Adding Slack account {id}"),
            Job::Llm { name, test, .. } => {
                if *test {
                    format!("Saving {name} and running a test call (this can take a minute)...")
                } else {
                    format!("Saving {name}...")
                }
            }
            Job::Estimate => "Estimating the first sync (`sb sync --estimate`)".into(),
            Job::Sync => "Running `sb sync` (Ctrl+C stops it; run it again to resume)".into(),
        }
    }
}

pub(crate) enum Outcome {
    None,
    Quit,
    Job(Job),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Status {
    pub text: String,
    pub error: bool,
}

pub(crate) struct App {
    pub cat: Catalog,
    pub home: String,
    pub default_home: bool,
    pub section: usize,
    cursors: [usize; Section::ALL.len()],
    pub data: Snapshot,
    pub schedule: ScheduleDraft,
    pub modals: Vec<Modal>,
    pub status: Option<Status>,
    /// Shown in the header: "setup" or "settings".
    pub mode: &'static str,
    /// Set while an action runs from a confirmation.
    confirmed: bool,
}

fn str_of(v: &Value, k: &str, d: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or(d).to_string()
}

/// A value as text for editing: strings as is, string lists comma-separated
/// (unless an item has a comma), anything else as JSON.
pub(crate) fn edit_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a)
            if a.iter()
                .all(|x| x.as_str().is_some_and(|s| !s.contains(','))) =>
        {
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        }
        Value::Null => String::new(),
        v => v.to_string(),
    }
}

/// Parse edited text back, shaped like `like` (the current or default value).
pub(crate) fn parse_edit(text: &str, like: Option<&Value>) -> Value {
    let t = text.trim();
    match like {
        Some(Value::String(_)) => Value::String(t.to_string()),
        Some(Value::Array(_)) if !t.starts_with('[') => Value::Array(
            t.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.to_string()))
                .collect(),
        ),
        _ => admin::parse_value(t),
    }
}

/// A value for a one-line listing.
pub(crate) fn show_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}

/// Fixed choices of a setting, if it has them.
fn setting_choices(key: &str, profiles: &[(String, Value)]) -> Option<Vec<String>> {
    let names = || profiles.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
    Some(match key {
        "display.language" => vec!["ja".into(), "en".into()],
        "summary.language" => vec!["auto".into(), "ja".into(), "en".into()],
        "notify.min_severity" => vec!["info".into(), "warning".into(), "error".into()],
        "summary.profile.default" if !profiles.is_empty() => names(),
        k if k.starts_with("summary.profile.") => {
            let mut n = names();
            n.insert(0, "native".into());
            n
        }
        _ => return None,
    })
}

fn account_choices(key: &str) -> Option<Vec<String>> {
    match key {
        "mention_scan" => Some(vec!["all".into(), "search_only".into(), "off".into()]),
        _ => None,
    }
}

impl App {
    pub fn new(ctx: &Ctx, start: Start) -> anyhow::Result<Self> {
        let cat = ctx.catalog()?;
        let mut app = App {
            cat,
            home: ctx.home.root().display().to_string(),
            default_home: ctx.home.is_default(),
            section: 0,
            cursors: [0; Section::ALL.len()],
            data: Snapshot::default(),
            schedule: ScheduleDraft {
                time: "19:30".into(),
                deep_day: "mon".into(),
                deep_time: "18:30".into(),
                systemd: false,
            },
            modals: Vec::new(),
            status: None,
            mode: "setup",
            confirmed: false,
        };
        app.reload()?;
        if let Some(reg) = &app.data.schedule {
            app.schedule = ScheduleDraft {
                time: str_of(reg, "time", "19:30"),
                deep_day: str_of(reg, "deep_day", "mon"),
                deep_time: str_of(reg, "deep_time", "18:30"),
                systemd: str_of(reg, "mechanism", "") == "systemd",
            };
        }
        match start {
            Start::Setup => {}
            Start::Settings => {
                app.mode = "settings";
                app.goto(Section::Settings);
            }
            Start::Account(id) => {
                app.mode = "settings";
                app.goto(Section::Accounts);
                app.modals
                    .push(Modal::Account(AccountEditor::load(&app.cat, &id)?));
            }
        }
        Ok(app)
    }

    /// Read the catalog again.
    pub fn reload(&mut self) -> anyhow::Result<()> {
        let cat = &self.cat;
        let mut profiles = Vec::new();
        for (k, v) in cat.settings_with_prefix("llm.profiles.")? {
            profiles.push((k.trim_start_matches("llm.profiles.").to_string(), v));
        }
        let user_home = skills::user_home().ok();
        let skills = Target::ALL
            .iter()
            .map(|t| SkillRow {
                target: *t,
                agent_found: t.is_installed_agent(),
                installed: user_home
                    .as_deref()
                    .and_then(|h| skills::installed_version(*t, h)),
            })
            .collect();
        let mut settings = cat.list_settings()?;
        settings.sort_by(|a, b| a.key.cmp(&b.key));
        self.data = Snapshot {
            accounts: cat
                .accounts()?
                .into_iter()
                .filter(|a| matches!(a.kind, AccountKind::Google | AccountKind::Slack))
                .collect(),
            default_profile: cat
                .setting("summary.profile.default")?
                .and_then(|v| v.as_str().map(str::to_string)),
            profiles,
            schedule: cat.setting("schedule.registered")?,
            skills,
            skill_version: skills::embedded_version(),
            initial_days: cat.setting_or("sync.initial_days", 30u64)?,
            settings,
            secrets: cat.list_secrets()?,
        };
        for m in &mut self.modals {
            if let Modal::Account(ed) = m
                && let Ok(fresh) = AccountEditor::load(&self.cat, &ed.id)
            {
                let cursor = ed.cursor.min(fresh.rows.len().saturating_sub(1));
                *ed = AccountEditor { cursor, ..fresh };
            }
        }
        Ok(())
    }

    pub fn current(&self) -> Section {
        Section::ALL[self.section]
    }

    pub fn goto(&mut self, s: Section) {
        if let Some(i) = Section::ALL.iter().position(|x| *x == s) {
            self.section = i;
        }
    }

    pub fn cursor(&self) -> usize {
        let n = self.rows(self.current()).len();
        self.cursors[self.section].min(n.saturating_sub(1))
    }

    /// Whether a section is complete, for the mark in the navigation column.
    pub fn done(&self, s: Section) -> Option<bool> {
        let d = &self.data;
        Some(match s {
            Section::Home => true,
            Section::Accounts => !d.accounts.is_empty(),
            Section::Summarizer => d
                .default_profile
                .as_ref()
                .is_some_and(|p| d.profiles.iter().any(|(n, _)| n == p)),
            Section::Schedule => d.schedule.is_some(),
            Section::Skills => d.skills.iter().any(|s| s.installed.is_some()),
            _ => return None,
        })
    }

    /// The rows of a section.
    pub fn rows(&self, s: Section) -> Vec<Row> {
        let d = &self.data;
        match s {
            Section::Home => {
                let mut rows = vec![
                    Row::new(format!("Directory   {}", self.home), Tone::Normal, None),
                    Row::new(
                        format!(
                            "Location    {}",
                            if self.default_home {
                                "default"
                            } else {
                                "custom (SECOND_BRAIN_HOME or --home)"
                            }
                        ),
                        Tone::Normal,
                        None,
                    ),
                    Row::new("Database    ready", Tone::Normal, None),
                ];
                if !self.default_home {
                    rows.push(Row::new(
                        "+ Persist SECOND_BRAIN_HOME so scheduled jobs and agents find it",
                        Tone::Action,
                        Some(Act::PersistEnv),
                    ));
                }
                rows
            }
            Section::Accounts => {
                let mut rows: Vec<Row> = d
                    .accounts
                    .iter()
                    .map(|a| {
                        let active = a.status != AccountStatus::Disabled;
                        Row::new(
                            format!(
                                "{:<18} {:<7} {:<13} {}",
                                a.id,
                                a.kind.to_string(),
                                a.status.to_string(),
                                a.identity.as_deref().unwrap_or("")
                            ),
                            match a.status {
                                AccountStatus::Active => Tone::Normal,
                                AccountStatus::Disabled => Tone::Dim,
                                _ => Tone::Warn,
                            },
                            Some(Act::EditAccount(a.id.to_string())),
                        )
                        .alt(Act::ToggleAccount(a.id.to_string(), active))
                    })
                    .collect();
                rows.push(Row::new(
                    "+ Add a Google account (Meet notes, Docs)",
                    Tone::Action,
                    Some(Act::AddGoogle),
                ));
                rows.push(Row::new(
                    "+ Add a Slack workspace",
                    Tone::Action,
                    Some(Act::AddSlack),
                ));
                rows
            }
            Section::Summarizer => {
                let mut rows: Vec<Row> = d
                    .profiles
                    .iter()
                    .map(|(name, p)| {
                        let default = d.default_profile.as_deref() == Some(name.as_str());
                        Row::new(
                            format!(
                                "{} {:<16} {:<16} {}",
                                if default { "*" } else { " " },
                                name,
                                str_of(p, "provider", ""),
                                str_of(p, "model", "(provider default)")
                            ),
                            Tone::Normal,
                            Some(Act::MakeDefault(name.clone())),
                        )
                        .alt(Act::RemoveProfile(name.clone()))
                    })
                    .collect();
                rows.push(Row::new(
                    "+ Add a summarizer",
                    Tone::Action,
                    Some(Act::AddProfile),
                ));
                rows
            }
            Section::Schedule => {
                let s = &self.schedule;
                let mut rows = vec![
                    Row::new(
                        format!("Daily sync time        {}", s.time),
                        Tone::Normal,
                        Some(Act::EditSchedule(SchedField::Time)),
                    ),
                    Row::new(
                        format!("Weekly deep sync day   {}", s.deep_day),
                        Tone::Normal,
                        Some(Act::EditSchedule(SchedField::DeepDay)),
                    ),
                    Row::new(
                        format!("Weekly deep sync time  {}", s.deep_time),
                        Tone::Normal,
                        Some(Act::EditSchedule(SchedField::DeepTime)),
                    ),
                ];
                if cfg!(target_os = "linux") {
                    rows.push(Row::new(
                        format!(
                            "Use systemd user timers {}",
                            if s.systemd { "yes" } else { "no (crontab)" }
                        ),
                        Tone::Normal,
                        Some(Act::ToggleSystemd),
                    ));
                }
                let registered = match &d.schedule {
                    Some(r) => format!(
                        "Registered: daily {} and {} {} ({})",
                        str_of(r, "time", "?"),
                        str_of(r, "deep_day", "?"),
                        str_of(r, "deep_time", "?"),
                        str_of(r, "mechanism", "?")
                    ),
                    None => "Not registered".into(),
                };
                rows.push(Row::new(
                    registered,
                    if d.schedule.is_some() {
                        Tone::Dim
                    } else {
                        Tone::Warn
                    },
                    None,
                ));
                rows.push(Row::new(
                    "+ Register the scheduled jobs",
                    Tone::Action,
                    Some(Act::RegisterSchedule),
                ));
                if d.schedule.is_some() {
                    rows.push(Row::new(
                        "- Remove the scheduled jobs",
                        Tone::Action,
                        Some(Act::RemoveSchedule),
                    ));
                }
                rows
            }
            Section::Skills => d
                .skills
                .iter()
                .map(|s| {
                    let state = match &s.installed {
                        Some(v) if *v == d.skill_version => format!("installed (v{v})"),
                        Some(v) => format!("installed v{v}, v{} available", d.skill_version),
                        None => "not installed".into(),
                    };
                    let mut row = Row::new(
                        format!(
                            "{:<8} {:<28} {}",
                            s.target.name(),
                            state,
                            if s.agent_found {
                                format!("`{}` found", s.target.binary())
                            } else {
                                format!("`{}` not found", s.target.binary())
                            }
                        ),
                        match (&s.installed, s.agent_found) {
                            (Some(v), _) if *v != d.skill_version => Tone::Warn,
                            (None, false) => Tone::Dim,
                            _ => Tone::Normal,
                        },
                        Some(Act::InstallSkill(s.target)),
                    );
                    if s.installed.is_some() {
                        row = row.alt(Act::RemoveSkill(s.target));
                    }
                    row
                })
                .collect(),
            Section::FirstSync => vec![
                Row::new(
                    format!("Days back for the first sync  {}", d.initial_days),
                    Tone::Normal,
                    Some(Act::EditInitialDays),
                ),
                Row::new(
                    "+ Estimate the first sync (sb sync --estimate)",
                    Tone::Action,
                    Some(Act::Estimate),
                ),
                Row::new(
                    "+ Run the first sync now (sb sync; leaves this screen)",
                    Tone::Action,
                    Some(Act::RunSync),
                ),
            ],
            Section::Settings => {
                let mut rows: Vec<Row> = d
                    .settings
                    .iter()
                    .map(|s| {
                        let read_only = READ_ONLY_SETTINGS.contains(&s.key.as_str());
                        let text = format!("{:<30} {}", s.key, show_value(&s.value));
                        if read_only {
                            return Row::new(
                                text,
                                Tone::Dim,
                                Some(Act::Info(format!("{} is maintained by sb", s.key))),
                            );
                        }
                        let row = Row::new(
                            text,
                            if s.default { Tone::Dim } else { Tone::Normal },
                            Some(Act::EditSetting(s.key.clone())),
                        );
                        if s.default {
                            row
                        } else {
                            row.alt(Act::ResetSetting(s.key.clone()))
                        }
                    })
                    .collect();
                rows.push(Row::new(
                    "+ Add a setting",
                    Tone::Action,
                    Some(Act::AddSetting),
                ));
                rows
            }
            Section::Secrets => {
                let mut rows: Vec<Row> = d
                    .secrets
                    .iter()
                    .map(|s| {
                        Row::new(
                            format!("{:<22} {:<26} {}", s.scope, s.name, s.masked),
                            Tone::Normal,
                            Some(Act::SetSecret(s.scope.clone(), s.name.clone())),
                        )
                        .alt(Act::RemoveSecret(s.scope.clone(), s.name.clone()))
                    })
                    .collect();
                rows.push(Row::new(
                    "+ Store a secret",
                    Tone::Action,
                    Some(Act::AddSecret),
                ));
                rows
            }
        }
    }

    fn ok(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: false,
        });
    }

    fn err(&mut self, e: &anyhow::Error) {
        self.status = Some(Status {
            text: format!("{e:#}"),
            error: true,
        });
    }

    /// Record the result of an action: a message, or the error.
    fn report(&mut self, r: anyhow::Result<String>) {
        match r {
            Ok(m) => self.ok(m),
            Err(e) => self.err(&e),
        }
        if let Err(e) = self.reload() {
            self.err(&e);
        }
    }

    pub fn on_key(&mut self, ctx: &Ctx, key: KeyEvent) -> Outcome {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Outcome::Quit;
        }
        if !self.modals.is_empty() {
            return self.on_modal_key(ctx, key);
        }
        self.status = None;
        let n = self.rows(self.current()).len();
        let cur = self.cursor();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Outcome::Quit,
            KeyCode::Tab | KeyCode::Right => self.section = (self.section + 1) % Section::ALL.len(),
            KeyCode::BackTab | KeyCode::Left => {
                self.section = (self.section + Section::ALL.len() - 1) % Section::ALL.len()
            }
            KeyCode::Char(c @ '1'..='9') => {
                let i = (c as usize) - ('1' as usize);
                if i < Section::ALL.len() {
                    self.section = i;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursors[self.section] = (cur + 1).min(n.saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.cursors[self.section] = cur.saturating_sub(1),
            KeyCode::PageDown => self.cursors[self.section] = (cur + 10).min(n.saturating_sub(1)),
            KeyCode::PageUp => self.cursors[self.section] = cur.saturating_sub(10),
            KeyCode::Home => self.cursors[self.section] = 0,
            KeyCode::End => self.cursors[self.section] = n.saturating_sub(1),
            KeyCode::Char('r') => {
                let r = self.reload().map(|_| "Reloaded.".to_string());
                self.report(r);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(act) = self
                    .rows(self.current())
                    .get(cur)
                    .and_then(|r| r.primary.clone())
                {
                    return self.act(ctx, act);
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if let Some(act) = self
                    .rows(self.current())
                    .get(cur)
                    .and_then(|r| r.secondary.clone())
                {
                    return self.act(ctx, act);
                }
            }
            _ => {}
        }
        Outcome::None
    }

    fn confirm(&mut self, text: impl Into<String>, act: Act) {
        self.modals.push(Modal::Confirm {
            text: text.into(),
            then: act,
        });
    }

    fn form(&mut self, form: Form, purpose: Purpose) {
        self.modals.push(Modal::Form(Box::new(form), purpose));
    }

    /// Run a row action (after confirmation where needed).
    fn act(&mut self, ctx: &Ctx, act: Act) -> Outcome {
        match act {
            Act::Info(m) => self.ok(m),
            Act::PersistEnv => {
                let r = setup::persist_env(ctx);
                self.report(r);
            }
            Act::EditAccount(id) => match AccountEditor::load(&self.cat, &id) {
                Ok(ed) => self.modals.push(Modal::Account(ed)),
                Err(e) => self.err(&e),
            },
            Act::ToggleAccount(id, active) => {
                let r = (|| {
                    let a = self.cat.account_by_str(&id)?.ok_or_else(|| {
                        crate::util::failure("account.not_found", format!("no account {id}"))
                    })?;
                    let status = if active {
                        AccountStatus::Disabled
                    } else {
                        AccountStatus::Active
                    };
                    self.cat.set_account_status(&a.id, status)?;
                    Ok(format!("{id} is now {status}."))
                })();
                self.report(r);
            }
            Act::AddGoogle => {
                let has_global = self
                    .cat
                    .secret(&SecretScope::Global, crate::factory::GOOGLE_CLIENT)
                    .ok()
                    .flatten()
                    .is_some();
                let form = Form::new(
                    "Add a Google account",
                    vec![
                        Field::text("id", "Account ID", "").help("For example work-google."),
                        Field::text(
                            "client",
                            "Client JSON",
                            if has_global { "global" } else { "" },
                        )
                        .help(
                            "Path to the OAuth client JSON file, or `global` for the stored one.",
                        ),
                        Field::text("features", "Features", "meet")
                            .help("Comma-separated: meet, docs, gmail."),
                        Field::text("label", "Label", "").help("Optional; default: the email."),
                    ],
                )
                .note("The Google consent runs in your browser. This screen pauses")
                .note("until it finishes, then comes back.")
                .submit_label("Sign in with Google");
                self.form(form, Purpose::Google);
            }
            Act::AddSlack => {
                let form = Form::new(
                    "Add a Slack workspace",
                    vec![
                        Field::text("id", "Account ID", "").help("For example acme-slack."),
                        Field::secret("token", "User token").help("The User OAuth Token, xoxp-..."),
                        Field::text("label", "Label", "")
                            .help("Optional; default: the workspace name."),
                    ],
                )
                .note("Create the app from assets/slack-app-manifest.yaml and install it.")
                .submit_label("Check the token and add");
                self.form(form, Purpose::Slack);
            }
            Act::MakeDefault(name) => {
                let r = self
                    .cat
                    .set_setting("summary.profile.default", &json!(name))
                    .map(|_| format!("{name} is the default summarizer."))
                    .map_err(Into::into);
                self.report(r);
            }
            Act::RemoveProfile(name) => {
                self.confirm(
                    format!("Remove the summarizer profile {name}?"),
                    Act::ResetSetting(format!("llm.profiles.{name}")),
                );
            }
            Act::AddProfile => {
                let options = second_brain_setup::llm::presets()
                    .iter()
                    .map(|p| p.label.to_string())
                    .collect();
                self.modals.push(Modal::Pick {
                    title: "Choose a summarizer".into(),
                    options,
                    cursor: 0,
                    purpose: PickPurpose::Preset,
                });
            }
            Act::EditSchedule(f) => {
                let (label, value, help) = match f {
                    SchedField::Time => ("Daily sync time", &self.schedule.time, "HH:MM"),
                    SchedField::DeepDay => (
                        "Weekly deep sync day",
                        &self.schedule.deep_day,
                        "mon, tue, wed, thu, fri, sat or sun",
                    ),
                    SchedField::DeepTime => {
                        ("Weekly deep sync time", &self.schedule.deep_time, "HH:MM")
                    }
                };
                let form = Form::new(label, vec![Field::text("v", label, value).help(help)]);
                self.form(form, Purpose::Schedule(f));
            }
            Act::ToggleSystemd => self.schedule.systemd = !self.schedule.systemd,
            Act::RegisterSchedule | Act::RemoveSchedule => {
                if act == Act::RemoveSchedule && !self.confirming() {
                    self.confirm("Remove the scheduled jobs?", Act::RemoveSchedule);
                    return Outcome::None;
                }
                let s = &self.schedule;
                let args = SetupScheduleArgs {
                    time: s.time.clone(),
                    deep_day: s.deep_day.clone(),
                    deep_time: s.deep_time.clone(),
                    systemd: s.systemd,
                    remove: act == Act::RemoveSchedule,
                    dry_run: false,
                };
                let r = setup::apply_schedule(ctx, &self.cat, &args).map(|(_, m)| m);
                self.report(r);
            }
            Act::InstallSkill(t) => {
                let r = setup::apply_skills(ctx, &[t], false).map(|d| {
                    d.iter()
                        .map(setup::skills_message)
                        .collect::<Vec<_>>()
                        .join("; ")
                });
                self.report(r);
            }
            Act::RemoveSkill(t) => {
                if !self.confirming() {
                    self.confirm(
                        format!("Remove the skill for {}?", t.name()),
                        Act::RemoveSkill(t),
                    );
                    return Outcome::None;
                }
                let r = setup::apply_skills(ctx, &[t], true).map(|d| {
                    d.iter()
                        .map(setup::skills_message)
                        .collect::<Vec<_>>()
                        .join("; ")
                });
                self.report(r);
            }
            Act::EditInitialDays => {
                let form = Form::new(
                    "First sync",
                    vec![
                        Field::text("v", "Days back", self.data.initial_days.to_string())
                            .help("A positive whole number (sync.initial_days)."),
                    ],
                );
                self.form(form, Purpose::InitialDays);
            }
            Act::Estimate => return Outcome::Job(Job::Estimate),
            Act::RunSync => return Outcome::Job(Job::Sync),
            Act::EditSetting(key) => {
                let current = self
                    .data
                    .settings
                    .iter()
                    .find(|s| s.key == key)
                    .map(|s| s.value.clone())
                    .unwrap_or(Value::Null);
                if let Value::Bool(b) = current {
                    let r = self.write_setting(&key, json!(!b));
                    self.report(r);
                } else if let Some(options) = setting_choices(&key, &self.data.profiles) {
                    let cursor = options
                        .iter()
                        .position(|o| current.as_str() == Some(o))
                        .unwrap_or(0);
                    self.modals.push(Modal::Pick {
                        title: key.clone(),
                        options,
                        cursor,
                        purpose: PickPurpose::Setting(key),
                    });
                } else {
                    let form = Form::new(
                        key.clone(),
                        vec![
                            Field::text("v", "Value", edit_text(&current))
                                .help(value_help(default_value(&key).as_ref().or(Some(&current)))),
                        ],
                    );
                    self.form(form, Purpose::Setting(key));
                }
            }
            Act::ResetSetting(key) => {
                if default_value(&key).is_none() && !self.confirming() {
                    self.confirm(
                        format!("Remove {key}? It has no default."),
                        Act::ResetSetting(key),
                    );
                    return Outcome::None;
                }
                let r = (|| {
                    self.cat.unset_setting(&key)?;
                    if let Some(name) = key.strip_prefix("llm.profiles.")
                        && self.data.default_profile.as_deref() == Some(name)
                    {
                        self.cat.unset_setting("summary.profile.default")?;
                    }
                    Ok(match default_value(&key) {
                        Some(d) => format!("{key} is back to the default {}.", show_value(&d)),
                        None => format!("Removed {key}."),
                    })
                })();
                self.report(r);
            }
            Act::AddSetting => {
                let form = Form::new(
                    "Add a setting",
                    vec![
                        Field::text("key", "Key", "").help(
                            "A dotted key, like google.meet.<...> or summary.profile.<source>.",
                        ),
                        Field::text("value", "Value", "").help("JSON, or text taken as a string."),
                    ],
                );
                self.form(form, Purpose::NewSetting);
            }
            Act::SetSecret(scope, name) => {
                let form = Form::new(
                    format!("{name} ({scope})"),
                    vec![Field::secret("v", "New value")],
                );
                self.form(form, Purpose::Secret(scope, name));
            }
            Act::RemoveSecret(scope, name) => {
                if !self.confirming() {
                    self.confirm(
                        format!("Delete the secret {name} ({scope})?"),
                        Act::RemoveSecret(scope, name),
                    );
                    return Outcome::None;
                }
                let r = (|| {
                    let s = SecretScope::parse(&scope)
                        .ok_or_else(|| crate::util::usage(format!("invalid scope {scope}")))?;
                    self.cat.delete_secret(&s, &name)?;
                    Ok(format!("Deleted {name} ({scope})."))
                })();
                self.report(r);
            }
            Act::AddSecret => {
                let form = Form::new(
                    "Store a secret",
                    vec![
                        Field::text("name", "Name", "").help("For example anthropic.api_key."),
                        Field::text("account", "Account", "")
                            .help("Optional account ID; empty for a global secret."),
                        Field::secret("value", "Value"),
                    ],
                );
                self.form(form, Purpose::NewSecret);
            }
        }
        Outcome::None
    }

    /// Whether the action runs from a confirmation ("yes" in the modal).
    fn confirming(&self) -> bool {
        self.confirmed
    }

    fn write_setting(&self, key: &str, v: Value) -> anyhow::Result<String> {
        admin::validate_setting(key, &v)?;
        self.cat.set_setting(key, &v)?;
        Ok(format!("{key} = {}", show_value(&v)))
    }

    fn write_account_value(&self, id: &str, key: &str, v: Option<Value>) -> anyhow::Result<String> {
        let a = self
            .cat
            .account_by_str(id)?
            .ok_or_else(|| crate::util::failure("account.not_found", format!("no account {id}")))?;
        let mut config = a.config.clone();
        if !config.is_object() {
            config = json!({});
        }
        let message = match &v {
            Some(v) => format!("{id}: {key} = {}", show_value(v)),
            None => format!("{id}: removed {key}"),
        };
        if let Some(o) = config.as_object_mut() {
            match v {
                Some(v) => {
                    o.insert(key.to_string(), v);
                }
                None => {
                    o.remove(key);
                }
            }
        }
        admin::validate_account_config(a.kind, &config)?;
        self.cat.update_account(&a.id, None, None, Some(&config))?;
        Ok(message)
    }

    fn on_modal_key(&mut self, ctx: &Ctx, key: KeyEvent) -> Outcome {
        let Some(top) = self.modals.last_mut() else {
            return Outcome::None;
        };
        match top {
            Modal::Confirm { then, .. } => {
                let then = then.clone();
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        self.modals.pop();
                        self.confirmed = true;
                        let out = self.act(ctx, then);
                        self.confirmed = false;
                        return out;
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        self.modals.pop();
                    }
                    _ => {}
                }
            }
            Modal::Pick {
                options,
                cursor,
                purpose,
                ..
            } => match key.code {
                KeyCode::Esc => {
                    self.modals.pop();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *cursor = (*cursor + 1).min(options.len().saturating_sub(1))
                }
                KeyCode::Up | KeyCode::Char('k') => *cursor = cursor.saturating_sub(1),
                KeyCode::Enter => {
                    let choice = options.get(*cursor).cloned().unwrap_or_default();
                    let index = *cursor;
                    let purpose = purpose.clone();
                    self.modals.pop();
                    self.picked(purpose, index, choice);
                }
                _ => {}
            },
            Modal::Form(form, purpose) => match form.handle(key) {
                FormEvent::None => {}
                FormEvent::Cancel => {
                    self.modals.pop();
                }
                FormEvent::Submit => {
                    let form = (**form).clone();
                    let purpose = purpose.clone();
                    match self.submitted(&form, purpose) {
                        Ok(Some(job)) => {
                            self.modals.pop();
                            return Outcome::Job(job);
                        }
                        Ok(None) => {
                            self.modals.pop();
                        }
                        // Keep the form open so the value can be fixed.
                        Err(e) => self.err(&e),
                    }
                }
            },
            Modal::Account(ed) => {
                let n = ed.rows.len();
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.modals.pop();
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        ed.cursor = (ed.cursor + 1).min(n.saturating_sub(1))
                    }
                    KeyCode::Up | KeyCode::Char('k') => ed.cursor = ed.cursor.saturating_sub(1),
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        let Some(row) = ed.rows.get(ed.cursor).cloned() else {
                            return Outcome::None;
                        };
                        let id = ed.id.clone();
                        self.status = None;
                        if READ_ONLY_ACCOUNT_KEYS.contains(&row.key.as_str()) {
                            self.ok(format!("{} is set by the sign-in", row.key));
                        } else if let Value::Bool(b) = row.value {
                            let r = self.write_account_value(&id, &row.key, Some(json!(!b)));
                            self.report(r);
                        } else if let Some(options) = account_choices(&row.key) {
                            let cursor = options
                                .iter()
                                .position(|o| row.value.as_str() == Some(o))
                                .unwrap_or(0);
                            self.modals.push(Modal::Pick {
                                title: format!("{id}: {}", row.key),
                                options,
                                cursor,
                                purpose: PickPurpose::AccountValue(id, row.key),
                            });
                        } else {
                            let form = Form::new(
                                format!("{id}: {}", row.key),
                                vec![
                                    Field::text("v", "Value", edit_text(&row.value))
                                        .help(value_help(Some(&row.value))),
                                ],
                            );
                            self.form(form, Purpose::AccountValue(id, row.key));
                        }
                    }
                    KeyCode::Char('x') | KeyCode::Delete => {
                        if let Some(row) = ed.rows.get(ed.cursor).cloned()
                            && !row.default
                        {
                            let id = ed.id.clone();
                            let r = self.write_account_value(&id, &row.key, None);
                            self.report(r);
                        }
                    }
                    _ => {}
                }
            }
        }
        Outcome::None
    }

    fn picked(&mut self, purpose: PickPurpose, index: usize, choice: String) {
        match purpose {
            PickPurpose::Setting(key) => {
                let r = self.write_setting(&key, json!(choice));
                self.report(r);
            }
            PickPurpose::AccountValue(id, key) => {
                let r = self.write_account_value(&id, &key, Some(json!(choice)));
                self.report(r);
            }
            PickPurpose::Preset => {
                let Some(preset) = second_brain_setup::llm::presets().into_iter().nth(index) else {
                    return;
                };
                let mut fields =
                    vec![
                    Field::text("name", "Profile name", preset.key).help(
                        "An existing profile of the same name is replaced (llm.profiles.<name>).",
                    ),
                    Field::text(
                        "model",
                        "Model",
                        preset.profile.get("model").and_then(Value::as_str).unwrap_or(""),
                    )
                    .help("Empty = the provider's default."),
                ];
                if preset.key == "local" {
                    fields.push(
                        Field::text(
                            "base_url",
                            "Base URL",
                            preset.profile["base_url"].as_str().unwrap_or(""),
                        )
                        .help("The OpenAI-compatible server."),
                    );
                }
                if let Some(secret) = preset.secret
                    && self
                        .cat
                        .secret_or_env(&SecretScope::Global, secret)
                        .ok()
                        .flatten()
                        .is_none()
                {
                    fields.push(
                        Field::secret("api_key", "API key")
                            .help(format!("Stored as {secret}. Empty = set it later.")),
                    );
                }
                fields.push(Field::toggle("default", "Make it the default", true));
                fields.push(
                    Field::toggle("test", "Run a test call", true)
                        .help("Sends a short request to check the setup."),
                );
                let mut form = Form::new(preset.label, fields).submit_label("Save the summarizer");
                if let Some(bin) = preset.binary
                    && second_brain_llm::resolve_program(bin).is_none()
                {
                    form = form.note(format!("warning: `{bin}` was not found on PATH"));
                }
                self.form(form, Purpose::Profile(Box::new(preset)));
            }
        }
    }

    /// Apply a submitted form. `Err` keeps the form open.
    fn submitted(&mut self, form: &Form, purpose: Purpose) -> anyhow::Result<Option<Job>> {
        let need = |v: String, what: &str| {
            if v.is_empty() {
                Err(crate::util::usage(format!("{what} is required")))
            } else {
                Ok(v)
            }
        };
        let optional = |v: String| if v.is_empty() { None } else { Some(v) };
        let message = match purpose {
            Purpose::Setting(key) => {
                let like = default_value(&key).or_else(|| {
                    self.data
                        .settings
                        .iter()
                        .find(|s| s.key == key)
                        .map(|s| s.value.clone())
                });
                self.write_setting(&key, parse_edit(&form.text("v"), like.as_ref()))?
            }
            Purpose::NewSetting => {
                let key = need(form.text("key"), "the key")?;
                let like = default_value(&key);
                self.write_setting(&key, parse_edit(&form.text("value"), like.as_ref()))?
            }
            Purpose::Secret(scope, name) => {
                let v = need(form.text("v"), "a value")?;
                let s = SecretScope::parse(&scope)
                    .ok_or_else(|| crate::util::usage(format!("invalid scope {scope}")))?;
                self.cat.set_secret(&s, &name, &Secret::new(v))?;
                format!("Stored {name} ({scope}).")
            }
            Purpose::NewSecret => {
                let name = need(form.text("name"), "the name")?;
                let v = need(form.text("value"), "a value")?;
                let scope = match optional(form.text("account")) {
                    Some(a) => SecretScope::Account(
                        self.cat
                            .account_by_str(&a)?
                            .ok_or_else(|| {
                                crate::util::failure("account.not_found", format!("no account {a}"))
                            })?
                            .id,
                    ),
                    None => SecretScope::Global,
                };
                self.cat.set_secret(&scope, &name, &Secret::new(v))?;
                format!("Stored {name} ({}).", scope.as_key())
            }
            Purpose::Google => {
                let id = need(form.text("id"), "the account ID")?;
                let client_secret = need(form.text("client"), "the client JSON")?;
                return Ok(Some(Job::AddGoogle {
                    id,
                    client_secret,
                    features: form
                        .text("features")
                        .split(',')
                        .map(|f| f.trim().to_string())
                        .filter(|f| !f.is_empty())
                        .collect(),
                    label: optional(form.text("label")),
                }));
            }
            Purpose::Slack => {
                let id = need(form.text("id"), "the account ID")?;
                let token = need(form.text("token"), "the token")?;
                return Ok(Some(Job::AddSlack {
                    id,
                    token: Secret::new(token),
                    label: optional(form.text("label")),
                }));
            }
            Purpose::Profile(preset) => {
                let name = need(form.text("name"), "the profile name")?;
                let base_url = optional(form.text("base_url"));
                let model = form.text("model");
                let profile = setup::preset_profile(&preset, Some(&model), base_url.as_deref());
                second_brain_llm::Profile::from_value(&name, &profile)
                    .map_err(|e| crate::util::usage(e.to_string()))?;
                if let (Some(secret), Some(key)) = (preset.secret, optional(form.text("api_key"))) {
                    self.cat
                        .set_secret(&SecretScope::Global, secret, &Secret::new(key))?;
                }
                return Ok(Some(Job::Llm {
                    preset,
                    name,
                    profile,
                    make_default: form.toggle("default"),
                    test: form.toggle("test"),
                }));
            }
            Purpose::Schedule(f) => {
                let v = form.text("v");
                let ok = match f {
                    SchedField::Time | SchedField::DeepTime => TimeOfDay::parse(&v).is_some(),
                    SchedField::DeepDay => Weekday::parse(&v).is_some(),
                };
                if !ok {
                    return Err(crate::util::usage(match f {
                        SchedField::DeepDay => format!("invalid day {v:?} (mon ... sun)"),
                        _ => format!("invalid time {v:?} (HH:MM)"),
                    }));
                }
                match f {
                    SchedField::Time => self.schedule.time = v,
                    SchedField::DeepDay => self.schedule.deep_day = v,
                    SchedField::DeepTime => self.schedule.deep_time = v,
                }
                "Changed. Register the scheduled jobs to apply it.".into()
            }
            Purpose::InitialDays => {
                let v = parse_edit(&form.text("v"), Some(&json!(30)));
                self.write_setting("sync.initial_days", v)?
            }
            Purpose::AccountValue(id, key) => {
                let like = self.modals.iter().rev().find_map(|m| match m {
                    Modal::Account(ed) if ed.id == id => ed
                        .rows
                        .iter()
                        .find(|r| r.key == key)
                        .map(|r| r.value.clone()),
                    _ => None,
                });
                let v = parse_edit(&form.text("v"), like.as_ref());
                self.write_account_value(&id, &key, Some(v))?
            }
        };
        self.report(Ok(message));
        Ok(None)
    }

    /// Show the result of a job that ran in the screen or the terminal.
    pub fn finished(&mut self, r: anyhow::Result<String>) {
        self.report(r);
    }
}

/// A hint about the expected value shape.
fn value_help(like: Option<&Value>) -> String {
    match like {
        Some(Value::Array(_)) => "A comma-separated list, or a JSON array.".into(),
        Some(Value::Number(_)) => "A number.".into(),
        Some(Value::Object(_)) => "A JSON object.".into(),
        Some(Value::String(_)) => "Text.".into(),
        _ => "JSON, or text taken as a string.".into(),
    }
}
