//! Parsing helpers, output helpers and errors with exit codes.

use std::io::{BufRead, IsTerminal, Write};
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, Utc};
use sb_store::EntryFilter;
use second_brain_kernel::{RawStatus, SectionKind, SourceKind, SummaryStatus};
use serde_json::Value;

use crate::cli::Filters;

/// Exit codes (cli.md).
pub mod exit {
    pub const OK: i32 = 0;
    pub const PROBLEMS: i32 = 1;
    pub const FAILURE: i32 = 2;
    pub const USAGE: i32 = 64;
    pub const LOCKED: i32 = 75;
    pub const INTERRUPTED: i32 = 130;
}

/// An error with a stable code and an exit status.
#[derive(Debug)]
pub struct CliError {
    pub code: &'static str,
    pub exit: i32,
    pub message: String,
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

pub fn usage(msg: impl Into<String>) -> anyhow::Error {
    CliError {
        code: "usage",
        exit: exit::USAGE,
        message: msg.into(),
    }
    .into()
}

pub fn failure(code: &'static str, msg: impl Into<String>) -> anyhow::Error {
    CliError {
        code,
        exit: exit::FAILURE,
        message: msg.into(),
    }
    .into()
}

/// Parse `YYYY-MM-DD` or RFC 3339. With `end_of_day`, a bare date means the
/// start of the next day (so `--until 2026-09-30` includes that day).
pub fn parse_date(s: &str, end_of_day: bool) -> anyhow::Result<DateTime<Utc>> {
    if let Some(t) = second_brain_kernel::util::parse_ts(s) {
        return Ok(t);
    }
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| usage(format!("invalid date {s:?} (use YYYY-MM-DD)")))?;
    let d = if end_of_day {
        d + ChronoDuration::days(1)
    } else {
        d
    };
    Ok(d.and_hms_opt(0, 0, 0)
        .map(|t| t.and_utc())
        .unwrap_or_default())
}

/// Parse a point in time for `sb sync --since/--until`: a date, an RFC 3339
/// time, or an age such as `90d` or `12w` (before `now`).
pub fn parse_when(s: &str, now: DateTime<Utc>) -> anyhow::Result<DateTime<Utc>> {
    let t = s.trim();
    if let Some((n, unit)) = t.split_at_checked(t.len().saturating_sub(1))
        && let Ok(n) = n.parse::<i64>()
        && n >= 0
    {
        let days = match unit {
            "d" => Some(n),
            "w" => n.checked_mul(7),
            _ => None,
        };
        if let Some(days) = days {
            return now
                .checked_sub_signed(ChronoDuration::days(days))
                .ok_or_else(|| usage(format!("age {s:?} is out of range")));
        }
    }
    parse_date(t, false).map_err(|_| {
        usage(format!(
            "invalid time {s:?} (use YYYY-MM-DD, an RFC 3339 time, or an age like 90d or 12w)"
        ))
    })
}

/// Parse durations like `90s`, `30m`, `2h`, `1h30m`.
pub fn parse_duration(s: &str) -> anyhow::Result<Duration> {
    let mut total = 0u64;
    let mut num = String::new();
    for c in s.trim().chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: u64 = num
            .parse()
            .map_err(|_| usage(format!("invalid duration {s:?}")))?;
        num.clear();
        total += match c {
            's' => n,
            'm' => n * 60,
            'h' => n * 3600,
            'd' => n * 86_400,
            _ => return Err(usage(format!("invalid duration {s:?} (use s, m, h or d)"))),
        };
    }
    if !num.is_empty() {
        // A bare number means minutes.
        total += num
            .parse::<u64>()
            .map_err(|_| usage(format!("invalid duration {s:?}")))?
            * 60;
    }
    if total == 0 {
        return Err(usage(format!("invalid duration {s:?}")));
    }
    Ok(Duration::from_secs(total))
}

pub fn parse_source_kinds(v: &[String]) -> anyhow::Result<Vec<SourceKind>> {
    v.iter()
        .map(|s| s.parse::<SourceKind>().map_err(|e| usage(e.to_string())))
        .collect()
}

pub fn parse_sections(v: &[String]) -> anyhow::Result<Vec<SectionKind>> {
    v.iter()
        .map(|s| s.parse::<SectionKind>().map_err(|e| usage(e.to_string())))
        .collect()
}

pub fn filter_from(f: &Filters) -> anyhow::Result<EntryFilter> {
    Ok(EntryFilter {
        accounts: f.accounts.clone(),
        source_kinds: parse_source_kinds(&f.sources)?,
        since: f
            .since
            .as_deref()
            .map(|s| parse_date(s, false))
            .transpose()?,
        until: f
            .until
            .as_deref()
            .map(|s| parse_date(s, true))
            .transpose()?,
        entry_uids: f.entries.clone(),
        raw_status: f
            .raw_status
            .iter()
            .map(|s| s.parse::<RawStatus>().map_err(|e| usage(e.to_string())))
            .collect::<anyhow::Result<_>>()?,
        summary_status: f
            .summary_status
            .iter()
            .map(|s| s.parse::<SummaryStatus>().map_err(|e| usage(e.to_string())))
            .collect::<anyhow::Result<_>>()?,
        ..Default::default()
    })
}

/// Print a JSON object as one line.
pub fn print_json(v: &Value) {
    println!("{}", serde_json::to_string(v).unwrap_or_default());
}

/// Whether stdin is interactive.
pub fn interactive() -> bool {
    std::io::stdin().is_terminal()
}

/// Ask a question on stderr and read a line from stdin.
pub fn ask(question: &str, default: Option<&str>) -> anyhow::Result<String> {
    let mut err = std::io::stderr();
    match default {
        Some(d) if !d.is_empty() => write!(err, "{question} [{d}]: ")?,
        _ => write!(err, "{question}: ")?,
    }
    err.flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let line = line.trim().to_string();
    Ok(if line.is_empty() {
        default.unwrap_or("").to_string()
    } else {
        line
    })
}

/// Yes/no question.
pub fn confirm(question: &str, default: bool) -> anyhow::Result<bool> {
    let hint = if default { "Y/n" } else { "y/N" };
    let a = ask(&format!("{question} ({hint})"), None)?;
    Ok(match a.to_lowercase().as_str() {
        "" => default,
        "y" | "yes" => true,
        _ => false,
    })
}

/// Numbered choice; returns the index.
pub fn choose(question: &str, options: &[String], default: usize) -> anyhow::Result<usize> {
    eprintln!("{question}");
    for (i, o) in options.iter().enumerate() {
        eprintln!("  {}) {o}", i + 1);
    }
    loop {
        let a = ask("Choose", Some(&(default + 1).to_string()))?;
        if let Ok(n) = a.parse::<usize>()
            && (1..=options.len()).contains(&n)
        {
            return Ok(n - 1);
        }
        eprintln!("Enter a number between 1 and {}.", options.len());
    }
}

/// Read a secret from a hidden prompt, or from stdin when not interactive.
pub fn read_secret(prompt: &str) -> anyhow::Result<String> {
    let v = if interactive() {
        rpassword::prompt_password(format!("{prompt}: "))?
    } else {
        let mut s = String::new();
        std::io::stdin().lock().read_line(&mut s)?;
        s
    };
    let v = v.trim().to_string();
    if v.is_empty() {
        return Err(usage("no value given"));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90s").unwrap().as_secs(), 90);
        assert_eq!(parse_duration("1h30m").unwrap().as_secs(), 5400);
        assert_eq!(parse_duration("15").unwrap().as_secs(), 900);
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("0m").is_err());
    }

    #[test]
    fn when_accepts_dates_and_ages() {
        let now = parse_date("2026-09-10", false).unwrap();
        let ts = second_brain_kernel::util::ts;
        assert_eq!(ts(parse_when("90d", now).unwrap()), "2026-06-12T00:00:00Z");
        assert_eq!(ts(parse_when("2w", now).unwrap()), "2026-08-27T00:00:00Z");
        assert_eq!(ts(parse_when("0d", now).unwrap()), "2026-09-10T00:00:00Z");
        assert_eq!(
            ts(parse_when("2026-07-01", now).unwrap()),
            "2026-07-01T00:00:00Z"
        );
        assert_eq!(
            ts(parse_when("2026-07-01T09:00:00+09:00", now).unwrap()),
            "2026-07-01T00:00:00Z"
        );
        assert!(parse_when("90x", now).is_err());
        assert!(parse_when("d", now).is_err());
        assert!(parse_when("-5d", now).is_err());
        assert!(parse_when("", now).is_err());
    }

    #[test]
    fn dates() {
        assert_eq!(
            second_brain_kernel::util::ts(parse_date("2026-09-30", false).unwrap()),
            "2026-09-30T00:00:00Z"
        );
        assert_eq!(
            second_brain_kernel::util::ts(parse_date("2026-09-30", true).unwrap()),
            "2026-10-01T00:00:00Z"
        );
        assert_eq!(
            second_brain_kernel::util::ts(parse_date("2026-09-30T12:00:00+09:00", true).unwrap()),
            "2026-09-30T03:00:00Z"
        );
        assert!(parse_date("30/09/2026", false).is_err());
    }
}
