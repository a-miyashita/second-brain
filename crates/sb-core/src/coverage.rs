//! Time-range planning for sync (ADR-0016): the forward step, extension
//! backwards, and explicit windows. Pure, so every source plans the same way.

use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

use crate::util::{parse_ts, ts};

/// Default length of the first window, in days (`sync.initial_days`).
pub const DEFAULT_INITIAL_DAYS: i64 = 30;
/// Default overlap kept before a stored forward cursor, in seconds
/// (`sync.overlap_secs`).
pub const DEFAULT_OVERLAP_SECS: i64 = 300;

/// The contiguous interval a scope has fetched: `[since, until]`. `since` is
/// `None` for cursors written before ADR-0016 ("unknown").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    pub since: Option<DateTime<Utc>>,
    /// The forward cursor.
    pub until: DateTime<Utc>,
}

impl Coverage {
    /// The start that is guaranteed: unknown counts as "nothing before `until`".
    pub fn guaranteed_since(&self) -> DateTime<Utc> {
        self.since.unwrap_or(self.until)
    }
}

/// Read `covered_since` from a cursor value.
pub fn covered_since_of(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("covered_since")
        .and_then(Value::as_str)
        .and_then(parse_ts)
}

/// `value` with `covered_since` set (other fields are kept).
pub fn with_covered_since(value: &Value, since: DateTime<Utc>) -> Value {
    let mut v = if value.is_object() {
        value.clone()
    } else {
        json!({})
    };
    v["covered_since"] = json!(ts(since));
    v
}

/// A half-open time range `[from, to)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// The backward part of a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackwardPlan {
    pub range: TimeRange,
    /// Whether finished windows extend `covered_since`. `false` for a detached
    /// explicit window, which would otherwise leave a hole in the coverage.
    pub record: bool,
}

/// What one run does for one scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Plan {
    pub forward: Option<TimeRange>,
    pub backward: Option<BackwardPlan>,
}

/// Plan one scope.
///
/// - `cov`: the stored coverage (`None` = no cursor yet).
/// - `initial_start`: `run_start - initial_days` (aligned by the source).
/// - `since` / `until`: `--since` / `--until`, already aligned by the source.
pub fn plan_ranges(
    cov: Option<Coverage>,
    run_start: DateTime<Utc>,
    initial_start: DateTime<Utc>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> Plan {
    let from = cov.map_or(initial_start, |c| c.until);
    let forward = (from < run_start).then_some(TimeRange {
        from,
        to: run_start,
    });
    let covered_since = cov.map_or(initial_start, |c| c.guaranteed_since());
    let backward = since.and_then(|x| {
        let y = until.map(|y| y.min(run_start));
        match y {
            None => (x < covered_since).then_some(BackwardPlan {
                range: TimeRange {
                    from: x,
                    to: covered_since,
                },
                record: true,
            }),
            Some(y) => (x < y).then_some(BackwardPlan {
                range: TimeRange { from: x, to: y },
                record: y >= covered_since && x < covered_since,
            }),
        }
    });
    Plan { forward, backward }
}

/// The forward cursor to store after fetching up to `run_start`: kept
/// `overlap` behind, but never behind where this run started.
pub fn next_cursor(
    from: DateTime<Utc>,
    run_start: DateTime<Utc>,
    overlap: Duration,
) -> DateTime<Utc> {
    (run_start - overlap).max(from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(day: i64) -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH + Duration::days(day)
    }

    fn cov(since: Option<i64>, until: i64) -> Option<Coverage> {
        Some(Coverage {
            since: since.map(t),
            until: t(until),
        })
    }

    #[test]
    fn first_run_covers_the_initial_window() {
        let p = plan_ranges(None, t(100), t(70), None, None);
        assert_eq!(
            p.forward,
            Some(TimeRange {
                from: t(70),
                to: t(100)
            })
        );
        assert_eq!(p.backward, None);
    }

    #[test]
    fn daily_run_continues_from_the_cursor() {
        let p = plan_ranges(cov(Some(70), 99), t(100), t(70), None, None);
        assert_eq!(
            p.forward,
            Some(TimeRange {
                from: t(99),
                to: t(100)
            })
        );
        assert_eq!(p.backward, None);
    }

    #[test]
    fn nothing_to_do_when_the_cursor_is_current() {
        let p = plan_ranges(cov(Some(70), 100), t(100), t(70), None, None);
        assert_eq!(p, Plan::default());
    }

    #[test]
    fn since_extends_to_the_covered_start_only() {
        let p = plan_ranges(cov(Some(70), 99), t(100), t(70), Some(t(10)), None);
        assert_eq!(
            p.backward,
            Some(BackwardPlan {
                range: TimeRange {
                    from: t(10),
                    to: t(70)
                },
                record: true
            })
        );
    }

    #[test]
    fn since_inside_the_covered_interval_is_skipped() {
        let p = plan_ranges(cov(Some(70), 99), t(100), t(70), Some(t(80)), None);
        assert_eq!(p.backward, None);
    }

    #[test]
    fn unknown_coverage_refetches_from_since_to_the_cursor() {
        let p = plan_ranges(cov(None, 99), t(100), t(70), Some(t(10)), None);
        assert_eq!(
            p.backward,
            Some(BackwardPlan {
                range: TimeRange {
                    from: t(10),
                    to: t(99)
                },
                record: true
            })
        );
    }

    #[test]
    fn first_run_with_since_extends_beyond_the_initial_window() {
        let p = plan_ranges(None, t(100), t(70), Some(t(40)), None);
        assert_eq!(
            p.forward,
            Some(TimeRange {
                from: t(70),
                to: t(100)
            })
        );
        assert_eq!(
            p.backward,
            Some(BackwardPlan {
                range: TimeRange {
                    from: t(40),
                    to: t(70)
                },
                record: true
            })
        );
    }

    #[test]
    fn explicit_window_touching_the_coverage_is_recorded() {
        let p = plan_ranges(cov(Some(70), 99), t(100), t(70), Some(t(50)), Some(t(80)));
        assert_eq!(
            p.backward,
            Some(BackwardPlan {
                range: TimeRange {
                    from: t(50),
                    to: t(80)
                },
                record: true
            })
        );
    }

    #[test]
    fn detached_explicit_window_is_not_recorded() {
        let p = plan_ranges(cov(Some(70), 99), t(100), t(70), Some(t(10)), Some(t(30)));
        assert_eq!(
            p.backward,
            Some(BackwardPlan {
                range: TimeRange {
                    from: t(10),
                    to: t(30)
                },
                record: false
            })
        );
    }

    #[test]
    fn explicit_window_inside_the_coverage_changes_nothing() {
        let p = plan_ranges(cov(Some(70), 99), t(100), t(70), Some(t(75)), Some(t(80)));
        assert!(!p.backward.unwrap().record);
    }

    #[test]
    fn until_is_clamped_to_the_run_start() {
        let p = plan_ranges(cov(Some(70), 99), t(100), t(70), Some(t(60)), Some(t(500)));
        assert_eq!(p.backward.unwrap().range.to, t(100));
    }

    #[test]
    fn empty_explicit_window_is_ignored() {
        let p = plan_ranges(cov(Some(70), 99), t(100), t(70), Some(t(50)), Some(t(50)));
        assert_eq!(p.backward, None);
    }

    #[test]
    fn cursor_keeps_an_overlap_but_never_goes_behind_the_run_start_point() {
        let o = Duration::minutes(5);
        assert_eq!(next_cursor(t(1), t(2), o), t(2) - o);
        let from = t(2) - Duration::minutes(1);
        assert_eq!(next_cursor(from, t(2), o), from);
    }

    #[test]
    fn covered_since_roundtrip() {
        let v = with_covered_since(&json!({"oldest": "1.0"}), t(3));
        assert_eq!(covered_since_of(&v), Some(t(3)));
        assert_eq!(v["oldest"], "1.0");
        assert_eq!(covered_since_of(&json!({})), None);
    }
}
