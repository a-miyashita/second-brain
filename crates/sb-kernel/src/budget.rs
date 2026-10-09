//! Budget periods (ADR-0013, summarization.md).
//!
//! A week starts on Monday 00:00 and a month on the 1st, 00:00, in a given time
//! zone. The periods are an internal accounting convention, not a billing cycle.
//! Everything here is pure: the current time is passed in.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Datelike, Days, Months, NaiveDate, NaiveTime, TimeZone, Utc};
pub use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// The length of a budget period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeriodKind {
    Week,
    Month,
}

impl PeriodKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PeriodKind::Week => "week",
            PeriodKind::Month => "month",
        }
    }
}

impl fmt::Display for PeriodKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PeriodKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "week" => Ok(PeriodKind::Week),
            "month" => Ok(PeriodKind::Month),
            _ => Err(()),
        }
    }
}

/// One budget period with its boundaries as UTC instants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Period {
    pub kind: PeriodKind,
    /// Local calendar date of the first day (a Monday, or the 1st).
    pub start: NaiveDate,
    /// Inclusive start.
    pub starts_at: DateTime<Utc>,
    /// Exclusive end; also the instant the next period starts.
    pub ends_at: DateTime<Utc>,
}

/// Whether `name` is an IANA time zone name.
pub fn is_valid_tz(name: &str) -> bool {
    name.parse::<Tz>().is_ok()
}

/// Parse an IANA time zone name; unknown names fall back to UTC.
pub fn parse_tz(name: &str) -> Tz {
    name.parse().unwrap_or(Tz::UTC)
}

/// The first local date of the period of `kind` that contains the local date `d`.
fn first_day(kind: PeriodKind, d: NaiveDate) -> NaiveDate {
    match kind {
        PeriodKind::Week => d - Days::new(u64::from(d.weekday().num_days_from_monday())),
        PeriodKind::Month => d.with_day(1).unwrap_or(d),
    }
}

/// The first local date of the period that follows the one starting on `start`.
pub fn next_start(kind: PeriodKind, start: NaiveDate) -> NaiveDate {
    match kind {
        PeriodKind::Week => start + Days::new(7),
        // `start` is the 1st, so adding a month cannot overflow the day.
        PeriodKind::Month => start + Months::new(1),
    }
}

/// The UTC instant of local midnight on `d`. A midnight that does not exist (a
/// DST gap) resolves to the first valid local time after it.
fn local_midnight(d: NaiveDate, tz: Tz) -> DateTime<Utc> {
    for hour in 0..=3 {
        let t = NaiveTime::from_hms_opt(hour, 0, 0).unwrap_or(NaiveTime::MIN);
        if let Some(local) = tz.from_local_datetime(&d.and_time(t)).earliest() {
            return local.with_timezone(&Utc);
        }
    }
    Utc.from_utc_datetime(&d.and_time(NaiveTime::MIN))
}

/// The period of `kind` that starts on the local date `start`.
pub fn period_starting(kind: PeriodKind, start: NaiveDate, tz: Tz) -> Period {
    Period {
        kind,
        start,
        starts_at: local_midnight(start, tz),
        ends_at: local_midnight(next_start(kind, start), tz),
    }
}

/// The period of `kind` that contains `now`.
pub fn period_containing(kind: PeriodKind, now: DateTime<Utc>, tz: Tz) -> Period {
    let local = now.with_timezone(&tz).date_naive();
    period_starting(kind, first_day(kind, local), tz)
}

/// The period immediately before `p`.
pub fn period_before(p: &Period, tz: Tz) -> Period {
    let start = match p.kind {
        PeriodKind::Week => p.start - Days::new(7),
        PeriodKind::Month => p.start - Months::new(1),
    };
    period_starting(p.kind, start, tz)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Weekday;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    const TOKYO: Tz = chrono_tz::Asia::Tokyo;

    #[test]
    fn week_starts_on_monday_in_the_given_zone() {
        // 2026-10-05 is a Monday.
        for (now, want) in [
            ("2026-10-05T00:00:00Z", date(2026, 10, 5)),
            ("2026-10-11T23:59:59Z", date(2026, 10, 5)),
            ("2026-10-12T00:00:00Z", date(2026, 10, 12)),
            ("2026-10-07T12:34:56Z", date(2026, 10, 5)),
        ] {
            let p = period_containing(PeriodKind::Week, utc(now), Tz::UTC);
            assert_eq!(p.start, want, "{now}");
            assert_eq!(p.start.weekday(), Weekday::Mon);
            assert_eq!(p.ends_at - p.starts_at, chrono::Duration::days(7));
            assert!(p.starts_at <= utc(now) && utc(now) < p.ends_at);
        }
    }

    #[test]
    fn local_date_differs_from_utc_date() {
        // Sunday 2026-10-11 20:00 UTC is already Monday 05:00 in Tokyo.
        let now = utc("2026-10-11T20:00:00Z");
        let utc_week = period_containing(PeriodKind::Week, now, Tz::UTC);
        let tokyo_week = period_containing(PeriodKind::Week, now, TOKYO);
        assert_eq!(utc_week.start, date(2026, 10, 5));
        assert_eq!(tokyo_week.start, date(2026, 10, 12));
        // Tokyo midnight is 15:00 UTC the day before.
        assert_eq!(tokyo_week.starts_at, utc("2026-10-11T15:00:00Z"));
        assert_eq!(tokyo_week.ends_at, utc("2026-10-18T15:00:00Z"));
    }

    #[test]
    fn month_boundaries() {
        let p = period_containing(PeriodKind::Month, utc("2026-10-31T23:59:59Z"), Tz::UTC);
        assert_eq!(p.start, date(2026, 10, 1));
        assert_eq!(p.ends_at, utc("2026-11-01T00:00:00Z"));
        let p = period_containing(PeriodKind::Month, utc("2026-11-01T00:00:00Z"), Tz::UTC);
        assert_eq!(p.start, date(2026, 11, 1));
        // Leap year February.
        let p = period_containing(PeriodKind::Month, utc("2028-02-29T10:00:00Z"), Tz::UTC);
        assert_eq!(p.start, date(2028, 2, 1));
        assert_eq!(p.ends_at, utc("2028-03-01T00:00:00Z"));
        // Year boundary.
        let p = period_containing(PeriodKind::Month, utc("2026-12-31T23:00:00Z"), Tz::UTC);
        assert_eq!(p.ends_at, utc("2027-01-01T00:00:00Z"));
    }

    #[test]
    fn week_spanning_a_year_boundary() {
        // 2026-12-31 is a Thursday; its week starts on 2026-12-28.
        let p = period_containing(PeriodKind::Week, utc("2026-12-31T12:00:00Z"), Tz::UTC);
        assert_eq!(p.start, date(2026, 12, 28));
        assert_eq!(p.ends_at, utc("2027-01-04T00:00:00Z"));
    }

    #[test]
    fn previous_periods_chain_without_gaps() {
        let mut p = period_containing(PeriodKind::Month, utc("2026-03-15T00:00:00Z"), TOKYO);
        for _ in 0..14 {
            let prev = period_before(&p, TOKYO);
            assert_eq!(prev.ends_at, p.starts_at);
            assert_eq!(prev.start.day(), 1);
            p = prev;
        }
        let mut w = period_containing(PeriodKind::Week, utc("2026-03-15T00:00:00Z"), TOKYO);
        for _ in 0..60 {
            let prev = period_before(&w, TOKYO);
            assert_eq!(prev.ends_at, w.starts_at);
            assert_eq!(prev.start.weekday(), Weekday::Mon);
            w = prev;
        }
    }

    #[test]
    fn dst_zone_week_is_not_always_168_hours() {
        // New York leaves DST on 2026-11-01, so that week has 169 hours.
        let ny: Tz = chrono_tz::America::New_York;
        let p = period_containing(PeriodKind::Week, utc("2026-11-02T12:00:00Z"), ny);
        assert_eq!(p.start, date(2026, 11, 2));
        let before = period_before(&p, ny);
        assert_eq!(before.start, date(2026, 10, 26));
        assert_eq!(
            before.ends_at - before.starts_at,
            chrono::Duration::hours(169)
        );
    }

    #[test]
    fn kind_round_trips() {
        for k in [PeriodKind::Week, PeriodKind::Month] {
            assert_eq!(k.as_str().parse::<PeriodKind>(), Ok(k));
        }
        assert!("year".parse::<PeriodKind>().is_err());
        assert_eq!(parse_tz("Nowhere/Land"), Tz::UTC);
        assert_eq!(parse_tz("Asia/Tokyo"), TOKYO);
    }
}
