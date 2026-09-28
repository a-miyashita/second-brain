//! Scheduled job registration (ADR-0009, setup-and-scheduling.md). Every
//! mechanism is rendered by a pure function, so tests check the output
//! without installing anything.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Result, SetupError, run};

/// Reverse-DNS prefix for launchd labels.
pub const LAUNCHD_PREFIX: &str = "com.github.a-miyashita.second-brain";
const CRON_BEGIN: &str = "# BEGIN second-brain";
const CRON_END: &str = "# END second-brain";

/// Day of week for the weekly deep sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().get(..3)? {
            "mon" => Weekday::Mon,
            "tue" => Weekday::Tue,
            "wed" => Weekday::Wed,
            "thu" => Weekday::Thu,
            "fri" => Weekday::Fri,
            "sat" => Weekday::Sat,
            "sun" => Weekday::Sun,
            _ => return None,
        })
    }

    /// cron / launchd number (Sunday = 0).
    fn number(self) -> u8 {
        match self {
            Weekday::Sun => 0,
            Weekday::Mon => 1,
            Weekday::Tue => 2,
            Weekday::Wed => 3,
            Weekday::Thu => 4,
            Weekday::Fri => 5,
            Weekday::Sat => 6,
        }
    }

    fn task_scheduler(self) -> &'static str {
        match self {
            Weekday::Mon => "Monday",
            Weekday::Tue => "Tuesday",
            Weekday::Wed => "Wednesday",
            Weekday::Thu => "Thursday",
            Weekday::Fri => "Friday",
            Weekday::Sat => "Saturday",
            Weekday::Sun => "Sunday",
        }
    }

    fn systemd(self) -> &'static str {
        match self {
            Weekday::Mon => "Mon",
            Weekday::Tue => "Tue",
            Weekday::Wed => "Wed",
            Weekday::Thu => "Thu",
            Weekday::Fri => "Fri",
            Weekday::Sat => "Sat",
            Weekday::Sun => "Sun",
        }
    }
}

/// A time of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TimeOfDay {
    pub hour: u8,
    pub minute: u8,
}

impl TimeOfDay {
    pub fn parse(s: &str) -> Option<Self> {
        let (h, m) = s.split_once(':')?;
        let (hour, minute) = (h.parse().ok()?, m.parse().ok()?);
        (hour < 24 && minute < 60).then_some(TimeOfDay { hour, minute })
    }
}

/// What to register.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScheduleSpec {
    pub binary: PathBuf,
    pub home: PathBuf,
    pub daily: TimeOfDay,
    pub deep_day: Weekday,
    pub deep_time: TimeOfDay,
    /// `PATH` for the jobs (Unix): LLM CLI directories plus system defaults.
    pub path: String,
}

impl Default for ScheduleSpec {
    fn default() -> Self {
        ScheduleSpec {
            binary: PathBuf::new(),
            home: PathBuf::new(),
            daily: TimeOfDay {
                hour: 19,
                minute: 30,
            },
            deep_day: Weekday::Mon,
            deep_time: TimeOfDay {
                hour: 18,
                minute: 30,
            },
            path: String::new(),
        }
    }
}

/// A registered job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Job {
    Sync,
    SyncDeep,
}

impl Job {
    pub const ALL: [Job; 2] = [Job::Sync, Job::SyncDeep];

    pub fn name(self) -> &'static str {
        match self {
            Job::Sync => "sync",
            Job::SyncDeep => "sync-deep",
        }
    }

    fn args(self) -> &'static [&'static str] {
        match self {
            Job::Sync => &["sync"],
            Job::SyncDeep => &["sync", "--deep"],
        }
    }
}

/// Build the job `PATH`: directories of the given executables, then defaults.
pub fn job_path(executables: &[PathBuf]) -> String {
    let mut dirs: Vec<String> = Vec::new();
    for e in executables {
        if let Some(d) = e.parent().map(|p| p.display().to_string())
            && !dirs.contains(&d)
        {
            dirs.push(d);
        }
    }
    let defaults: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ]
    } else {
        &["/usr/local/bin", "/usr/bin", "/bin"]
    };
    for d in defaults {
        if !dirs.iter().any(|x| x == d) {
            dirs.push(d.to_string());
        }
    }
    dirs.join(":")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn win_quote(s: &str) -> String {
    format!("\"{s}\"")
}

// ---------- Windows Task Scheduler ----------

/// Task name (`\second-brain\sync`).
pub fn task_name(job: Job) -> String {
    format!("\\second-brain\\{}", job.name())
}

/// Render the Task Scheduler XML. The job runs `conhost.exe --headless` so no
/// console window flashes, and passes the trigger with `--trigger schedule`
/// because Task Scheduler actions cannot set environment variables.
pub fn render_task_xml(spec: &ScheduleSpec, job: Job) -> String {
    let (time, schedule) = match job {
        Job::Sync => (
            spec.daily,
            "<ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay>".to_string(),
        ),
        Job::SyncDeep => (
            spec.deep_time,
            format!(
                "<ScheduleByWeek><DaysOfWeek><{0} /></DaysOfWeek><WeeksInterval>1</WeeksInterval></ScheduleByWeek>",
                spec.deep_day.task_scheduler()
            ),
        ),
    };
    let args = format!(
        "--headless {} --home {} --trigger schedule {}",
        win_quote(&spec.binary.display().to_string()),
        win_quote(&spec.home.display().to_string()),
        job.args().join(" ")
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>second-brain {name}</Description>
  </RegistrationInfo>
  <Triggers>
    <CalendarTrigger>
      <StartBoundary>2026-01-01T{h:02}:{m:02}:00</StartBoundary>
      <Enabled>true</Enabled>
      {schedule}
    </CalendarTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <ExecutionTimeLimit>PT6H</ExecutionTimeLimit>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>conhost.exe</Command>
      <Arguments>{args}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        name = job.name(),
        h = time.hour,
        m = time.minute,
        schedule = schedule,
        args = xml_escape(&args),
    )
}

/// `schtasks` needs the XML file in UTF-16 LE with a BOM.
pub fn utf16le_with_bom(s: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

// ---------- macOS launchd ----------

pub fn launchd_label(job: Job) -> String {
    format!("{LAUNCHD_PREFIX}.{}", job.name())
}

pub fn launchd_plist_path(user_home: &Path, job: Job) -> PathBuf {
    user_home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", launchd_label(job)))
}

pub fn render_launchd_plist(spec: &ScheduleSpec, job: Job) -> String {
    let mut args = vec![
        spec.binary.display().to_string(),
        "--home".to_string(),
        spec.home.display().to_string(),
    ];
    args.extend(job.args().iter().map(|s| s.to_string()));
    let args_xml: String = args
        .iter()
        .map(|a| format!("    <string>{}</string>\n", xml_escape(a)))
        .collect();
    let (time, weekday) = match job {
        Job::Sync => (spec.daily, String::new()),
        Job::SyncDeep => (
            spec.deep_time,
            format!(
                "    <key>Weekday</key>\n    <integer>{}</integer>\n",
                spec.deep_day.number()
            ),
        ),
    };
    let log = spec
        .home
        .join("logs")
        .join(format!("launchd-{}.log", job.name()));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{args_xml}  </array>
  <key>StartCalendarInterval</key>
  <dict>
    <key>Hour</key>
    <integer>{h}</integer>
    <key>Minute</key>
    <integer>{m}</integer>
{weekday}  </dict>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
    <key>SB_TRIGGER</key>
    <string>schedule</string>
  </dict>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        label = launchd_label(job),
        h = time.hour,
        m = time.minute,
        path = xml_escape(&spec.path),
        log = xml_escape(&log.display().to_string()),
    )
}

// ---------- Linux crontab ----------

/// The crontab block between the markers.
pub fn render_cron_block(spec: &ScheduleSpec) -> String {
    let line = |t: TimeOfDay, dow: &str, job: Job| {
        format!(
            "{} {} * * {dow} SB_TRIGGER=schedule PATH={} {} --home {} {} >/dev/null 2>&1",
            t.minute,
            t.hour,
            sh_quote(&spec.path),
            sh_quote(&spec.binary.display().to_string()),
            sh_quote(&spec.home.display().to_string()),
            job.args().join(" ")
        )
    };
    format!(
        "{CRON_BEGIN}\n{}\n{}\n{CRON_END}\n",
        line(spec.daily, "*", Job::Sync),
        line(
            spec.deep_time,
            &spec.deep_day.number().to_string(),
            Job::SyncDeep
        )
    )
}

/// Replace (or remove, with `block = None`) the marked block in a crontab.
pub fn replace_cron_block(existing: &str, block: Option<&str>) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in existing.lines() {
        if line.trim() == CRON_BEGIN {
            inside = true;
            continue;
        }
        if line.trim() == CRON_END {
            inside = false;
            continue;
        }
        if !inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    if let Some(b) = block {
        if !out.is_empty() && !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str(b);
    }
    out
}

/// The marked block of a crontab, if any.
pub fn cron_block(existing: &str) -> Option<String> {
    let start = existing.find(CRON_BEGIN)?;
    let end = existing[start..].find(CRON_END)? + start + CRON_END.len();
    Some(existing[start..end].to_string())
}

// ---------- Linux systemd user timers ----------

pub fn systemd_dir(user_home: &Path) -> PathBuf {
    user_home.join(".config").join("systemd").join("user")
}

fn unit_base(job: Job) -> String {
    format!("second-brain-{}", job.name())
}

/// Render `(service file name, service, timer file name, timer)`.
pub fn render_systemd(spec: &ScheduleSpec, job: Job) -> (String, String, String, String) {
    let base = unit_base(job);
    let exec = format!(
        "{} --home {} {}",
        sh_quote(&spec.binary.display().to_string()),
        sh_quote(&spec.home.display().to_string()),
        job.args().join(" ")
    );
    let service = format!(
        "[Unit]\nDescription=second-brain {name}\n\n[Service]\nType=oneshot\nEnvironment=SB_TRIGGER=schedule\nEnvironment=\"PATH={path}\"\nExecStart={exec}\n",
        name = job.name(),
        path = spec.path,
    );
    let on_calendar = match job {
        Job::Sync => format!("*-*-* {:02}:{:02}:00", spec.daily.hour, spec.daily.minute),
        Job::SyncDeep => format!(
            "{} *-*-* {:02}:{:02}:00",
            spec.deep_day.systemd(),
            spec.deep_time.hour,
            spec.deep_time.minute
        ),
    };
    let timer = format!(
        "[Unit]\nDescription=second-brain {name} timer\n\n[Timer]\nOnCalendar={on_calendar}\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n",
        name = job.name()
    );
    (
        format!("{base}.service"),
        service,
        format!("{base}.timer"),
        timer,
    )
}

// ---------- Registration ----------

/// Mechanism used on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mechanism {
    TaskScheduler,
    Launchd,
    Crontab,
    Systemd,
}

impl Mechanism {
    /// The default for this OS (`systemd` only when requested on Linux).
    pub fn for_os(systemd: bool) -> Self {
        if cfg!(windows) {
            Mechanism::TaskScheduler
        } else if cfg!(target_os = "macos") {
            Mechanism::Launchd
        } else if systemd {
            Mechanism::Systemd
        } else {
            Mechanism::Crontab
        }
    }
}

/// Everything that would be written, for `--dry-run` and tests.
pub fn render_all(spec: &ScheduleSpec, mechanism: Mechanism) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for job in Job::ALL {
        match mechanism {
            Mechanism::TaskScheduler => out.push((task_name(job), render_task_xml(spec, job))),
            Mechanism::Launchd => out.push((launchd_label(job), render_launchd_plist(spec, job))),
            Mechanism::Systemd => {
                let (sn, s, tn, t) = render_systemd(spec, job);
                out.push((sn, s));
                out.push((tn, t));
            }
            Mechanism::Crontab => {}
        }
    }
    if mechanism == Mechanism::Crontab {
        out.push(("crontab".into(), render_cron_block(spec)));
    }
    out
}

fn write(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SetupError::io(parent, e))?;
    }
    std::fs::write(path, content).map_err(|e| SetupError::io(path, e))
}

fn current_uid() -> Result<String> {
    Ok(run("id", &["-u"], None)?.trim().to_string())
}

/// Register the jobs.
pub fn register(
    spec: &ScheduleSpec,
    mechanism: Mechanism,
    user_home: &Path,
    scratch: &Path,
) -> Result<()> {
    match mechanism {
        Mechanism::TaskScheduler => {
            for job in Job::ALL {
                let xml = scratch.join(format!("task-{}.xml", job.name()));
                write(&xml, &utf16le_with_bom(&render_task_xml(spec, job)))?;
                let res = run(
                    "schtasks",
                    &[
                        "/Create",
                        "/XML",
                        &xml.display().to_string(),
                        "/TN",
                        &task_name(job),
                        "/F",
                    ],
                    None,
                );
                let _ = std::fs::remove_file(&xml);
                res?;
            }
        }
        Mechanism::Launchd => {
            let uid = current_uid()?;
            for job in Job::ALL {
                let p = launchd_plist_path(user_home, job);
                write(&p, render_launchd_plist(spec, job).as_bytes())?;
                let target = format!("gui/{uid}");
                let _ = run(
                    "launchctl",
                    &["bootout", &target, &p.display().to_string()],
                    None,
                );
                run(
                    "launchctl",
                    &["bootstrap", &target, &p.display().to_string()],
                    None,
                )?;
            }
        }
        Mechanism::Crontab => {
            let existing = run("crontab", &["-l"], None).unwrap_or_default();
            let new = replace_cron_block(&existing, Some(&render_cron_block(spec)));
            run("crontab", &["-"], Some(&new))?;
        }
        Mechanism::Systemd => {
            let dir = systemd_dir(user_home);
            for job in Job::ALL {
                let (sn, s, tn, t) = render_systemd(spec, job);
                write(&dir.join(&sn), s.as_bytes())?;
                write(&dir.join(&tn), t.as_bytes())?;
            }
            run("systemctl", &["--user", "daemon-reload"], None)?;
            for job in Job::ALL {
                run(
                    "systemctl",
                    &[
                        "--user",
                        "enable",
                        "--now",
                        &format!("{}.timer", unit_base(job)),
                    ],
                    None,
                )?;
            }
        }
    }
    Ok(())
}

/// Remove the jobs. Missing jobs are not an error.
pub fn unregister(mechanism: Mechanism, user_home: &Path) -> Result<()> {
    match mechanism {
        Mechanism::TaskScheduler => {
            for job in Job::ALL {
                let _ = run("schtasks", &["/Delete", "/TN", &task_name(job), "/F"], None);
            }
        }
        Mechanism::Launchd => {
            let uid = current_uid()?;
            for job in Job::ALL {
                let p = launchd_plist_path(user_home, job);
                let _ = run(
                    "launchctl",
                    &["bootout", &format!("gui/{uid}"), &p.display().to_string()],
                    None,
                );
                if p.exists() {
                    std::fs::remove_file(&p).map_err(|e| SetupError::io(&p, e))?;
                }
            }
        }
        Mechanism::Crontab => {
            let existing = run("crontab", &["-l"], None).unwrap_or_default();
            if cron_block(&existing).is_some() {
                run(
                    "crontab",
                    &["-"],
                    Some(&replace_cron_block(&existing, None)),
                )?;
            }
        }
        Mechanism::Systemd => {
            let dir = systemd_dir(user_home);
            for job in Job::ALL {
                let base = unit_base(job);
                let _ = run(
                    "systemctl",
                    &["--user", "disable", "--now", &format!("{base}.timer")],
                    None,
                );
                for f in [format!("{base}.service"), format!("{base}.timer")] {
                    let p = dir.join(f);
                    if p.exists() {
                        std::fs::remove_file(&p).map_err(|e| SetupError::io(&p, e))?;
                    }
                }
            }
            let _ = run("systemctl", &["--user", "daemon-reload"], None);
        }
    }
    Ok(())
}

/// Check that the jobs exist and point to this binary and home. Returns the
/// problems found (empty when OK).
pub fn check(spec: &ScheduleSpec, mechanism: Mechanism, user_home: &Path) -> Vec<String> {
    let bin = spec.binary.display().to_string();
    let home = spec.home.display().to_string();
    let mut problems = Vec::new();
    match mechanism {
        Mechanism::TaskScheduler => {
            for job in Job::ALL {
                match run(
                    "schtasks",
                    &["/Query", "/TN", &task_name(job), "/XML"],
                    None,
                ) {
                    Ok(xml) if xml.contains(&bin) && xml.contains(&home) => {}
                    Ok(_) => problems.push(format!(
                        "task {} points to another binary or home",
                        task_name(job)
                    )),
                    Err(_) => problems.push(format!("task {} is not registered", task_name(job))),
                }
            }
        }
        Mechanism::Launchd => {
            for job in Job::ALL {
                let p = launchd_plist_path(user_home, job);
                match std::fs::read_to_string(&p) {
                    Ok(s) if s.contains(&xml_escape(&bin)) && s.contains(&xml_escape(&home)) => {}
                    Ok(_) => {
                        problems.push(format!("{} points to another binary or home", p.display()))
                    }
                    Err(_) => problems.push(format!("{} is missing", p.display())),
                }
            }
        }
        Mechanism::Crontab => match run("crontab", &["-l"], None)
            .ok()
            .as_deref()
            .and_then(cron_block)
        {
            Some(b) if b.contains(&bin) && b.contains(&home) => {}
            Some(_) => problems.push("the crontab block points to another binary or home".into()),
            None => problems.push("no second-brain block in the crontab".into()),
        },
        Mechanism::Systemd => {
            let dir = systemd_dir(user_home);
            for job in Job::ALL {
                let (sn, _, _, _) = render_systemd(spec, job);
                match std::fs::read_to_string(dir.join(&sn)) {
                    Ok(s) if s.contains(&bin) && s.contains(&home) => {}
                    Ok(_) => problems.push(format!("{sn} points to another binary or home")),
                    Err(_) => problems.push(format!("{sn} is missing")),
                }
            }
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ScheduleSpec {
        ScheduleSpec {
            binary: PathBuf::from("/home/u/.local/bin/second-brain"),
            home: PathBuf::from("/home/u/.local/share/second-brain"),
            path: job_path(&[PathBuf::from("/home/u/.local/bin/claude")]),
            ..Default::default()
        }
    }

    #[test]
    fn parse_times() {
        assert_eq!(
            TimeOfDay::parse("07:05"),
            Some(TimeOfDay { hour: 7, minute: 5 })
        );
        assert_eq!(TimeOfDay::parse("24:00"), None);
        assert_eq!(Weekday::parse("Monday"), Some(Weekday::Mon));
        assert_eq!(Weekday::parse("x"), None);
    }

    #[test]
    fn task_xml() {
        let x = render_task_xml(&spec(), Job::SyncDeep);
        assert!(x.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(x.contains("<StartWhenAvailable>true</StartWhenAvailable>"));
        assert!(x.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"));
        assert!(x.contains("<ExecutionTimeLimit>PT6H</ExecutionTimeLimit>"));
        assert!(x.contains("<Monday />"));
        assert!(x.contains("T18:30:00"));
        assert!(x.contains("--headless &quot;/home/u/.local/bin/second-brain&quot; --home"));
        assert!(x.contains("--trigger schedule sync --deep"));
        let b = utf16le_with_bom("a");
        assert_eq!(b, vec![0xFF, 0xFE, b'a', 0]);
    }

    #[test]
    fn launchd_plist() {
        let p = render_launchd_plist(&spec(), Job::SyncDeep);
        assert!(p.contains("<string>com.github.a-miyashita.second-brain.sync-deep</string>"));
        assert!(p.contains("<key>Weekday</key>\n    <integer>1</integer>"));
        assert!(p.contains("<string>--deep</string>"));
        assert!(p.contains("<key>SB_TRIGGER</key>\n    <string>schedule</string>"));
        assert!(p.contains("/home/u/.local/bin:"));
        let daily = render_launchd_plist(&spec(), Job::Sync);
        assert!(!daily.contains("Weekday"));
        assert!(daily.contains("<integer>19</integer>"));
    }

    #[test]
    fn cron_block_replacement_keeps_other_lines() {
        let block = render_cron_block(&spec());
        assert!(block.contains("30 19 * * * SB_TRIGGER=schedule PATH='/home/u/.local/bin:"));
        assert!(block.contains("30 18 * * 1 "));
        assert!(block.contains("sync --deep >/dev/null 2>&1"));
        let existing = "MAILTO=\"\"\n0 1 * * * backup\n";
        let installed = replace_cron_block(existing, Some(&block));
        assert!(installed.starts_with("MAILTO=\"\"\n0 1 * * * backup\n\n# BEGIN second-brain\n"));
        // Re-registering replaces only the block.
        let again = replace_cron_block(&installed, Some(&block));
        assert_eq!(again, installed);
        assert_eq!(cron_block(&installed).unwrap().lines().count(), 4);
        let removed = replace_cron_block(&installed, None);
        assert!(!removed.contains("second-brain"));
        assert!(removed.contains("backup"));
    }

    #[test]
    fn systemd_units() {
        let (sn, s, tn, t) = render_systemd(&spec(), Job::SyncDeep);
        assert_eq!(sn, "second-brain-sync-deep.service");
        assert_eq!(tn, "second-brain-sync-deep.timer");
        assert!(s.contains("Environment=SB_TRIGGER=schedule"));
        assert!(s.contains("sync --deep"));
        assert!(t.contains("OnCalendar=Mon *-*-* 18:30:00"));
        assert!(t.contains("Persistent=true"));
        assert_eq!(render_all(&spec(), Mechanism::Systemd).len(), 4);
    }
}
