//! The summarization budget (ADR-0013, summarization.md).
//!
//! [`BudgetGate`] decides before each paid call whether it fits in the weekly and
//! monthly caps. [`status`] and [`report`] read the ledger for display. The spend
//! of a period is always summed from the ledger (`llm_usage`).

use std::sync::Mutex;

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use second_brain_kernel::budget::{
    Period, PeriodKind, next_start, parse_tz, period_before, period_containing,
};
use second_brain_store::{Catalog, ModelSpend, PeriodRow, Spend};
use serde::Serialize;

use crate::error::PipelineError;
use crate::policy::SummaryPolicy;

/// Floating-point slack when comparing sums of small amounts.
const EPS: f64 = 1e-9;

/// The caps in force. `None` = disabled.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Caps {
    pub weekly_usd: Option<f64>,
    pub monthly_usd: Option<f64>,
}

impl Caps {
    pub fn of(&self, kind: PeriodKind) -> Option<f64> {
        match kind {
            PeriodKind::Week => self.weekly_usd,
            PeriodKind::Month => self.monthly_usd,
        }
    }

    pub fn any(&self) -> bool {
        self.weekly_usd.is_some() || self.monthly_usd.is_some()
    }
}

/// Why a unit of work cannot start.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Blocked {
    pub kind: PeriodKind,
    pub cap_usd: f64,
    pub spent_usd: f64,
    pub period_start: NaiveDate,
    /// When the period rolls over (raising the cap also unblocks).
    pub resets_at: DateTime<Utc>,
}

impl Blocked {
    /// The stop detail recorded on the run: `budget.weekly` or `budget.monthly`.
    pub fn stop_detail(&self) -> String {
        match self.kind {
            PeriodKind::Week => "budget.weekly".into(),
            PeriodKind::Month => "budget.monthly".into(),
        }
    }

    pub fn message(&self) -> String {
        format!(
            "the {} summarization budget is used up (${:.2} of ${:.2}); pending summaries resume after {} or when the cap is raised (sb config set summary.budget.{}_usd <amount>)",
            self.kind,
            self.spent_usd,
            self.cap_usd,
            self.resets_at.format("%Y-%m-%d %H:%M UTC"),
            match self.kind {
                PeriodKind::Week => "weekly",
                PeriodKind::Month => "monthly",
            },
        )
    }
}

/// The result of asking the gate.
#[derive(Debug)]
pub enum Gate<'g> {
    /// Go ahead. Dropping the reservation releases the in-flight estimate.
    Go(Reservation<'g>),
    /// A cap is used up: stop the stage.
    Stop(Blocked),
    /// The unit alone costs more than a whole period's cap: it can never run
    /// under this budget. Skip it and keep going.
    TooLarge {
        kind: PeriodKind,
        needed_usd: f64,
        cap_usd: f64,
    },
}

/// An estimate held for a call in flight.
#[derive(Debug)]
pub struct Reservation<'g> {
    gate: &'g BudgetGate,
    amount: f64,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if let Ok(mut r) = self.gate.reserved.lock() {
            *r = (*r - self.amount).max(0.0);
        }
    }
}

/// Decides whether paid calls fit in the budget.
#[derive(Debug)]
pub struct BudgetGate {
    caps: Caps,
    tz: Tz,
    /// Estimated cost of the calls in flight in this run.
    reserved: Mutex<f64>,
}

impl BudgetGate {
    pub fn new(caps: Caps, tz: Tz) -> Self {
        BudgetGate {
            caps,
            tz,
            reserved: Mutex::new(0.0),
        }
    }

    pub fn from_policy(p: &SummaryPolicy) -> Self {
        Self::new(
            Caps {
                weekly_usd: p.weekly_cap_usd,
                monthly_usd: p.monthly_cap_usd,
            },
            parse_tz(&p.budget_timezone),
        )
    }

    pub fn caps(&self) -> Caps {
        self.caps
    }

    pub fn enabled(&self) -> bool {
        self.caps.any()
    }

    fn periods(&self, cat: &Catalog) -> [Period; 2] {
        let now = cat.now();
        [PeriodKind::Week, PeriodKind::Month].map(|k| period_containing(k, now, self.tz))
    }

    /// Create or refresh the snapshots of the current week and month.
    pub fn record_periods(&self, cat: &Catalog) -> Result<(), PipelineError> {
        for p in self.periods(cat) {
            cat.upsert_period(&p, self.caps.of(p.kind))?;
        }
        Ok(())
    }

    /// Note that a cap stopped a run in the period of `b`.
    pub fn record_stop(&self, cat: &Catalog, b: &Blocked) -> Result<(), PipelineError> {
        cat.mark_period_stopped(b.kind, b.period_start)?;
        Ok(())
    }

    /// The money left under the strictest cap right now, without the in-flight
    /// reservations. `None` when no cap is enabled. Read-only.
    pub fn remaining(&self, cat: &Catalog) -> Result<Option<f64>, PipelineError> {
        let mut rem: Option<f64> = None;
        for p in self.periods(cat) {
            if let Some(cap) = self.caps.of(p.kind) {
                let spent = cat.spend_between(p.starts_at, p.ends_at)?.cost_usd;
                let left = (cap - spent).max(0.0);
                rem = Some(rem.map_or(left, |r| r.min(left)));
            }
        }
        Ok(rem)
    }

    /// Ask to start a unit whose estimated cost is `needed_usd`.
    ///
    /// The unit fits if, in every enabled period, the spend so far plus the
    /// calls in flight plus this unit stays within the cap, and some budget is
    /// left at all (a unit with an unknown, zero estimate does not start on an
    /// exhausted budget).
    pub fn try_reserve(&self, cat: &Catalog, needed_usd: f64) -> Result<Gate<'_>, PipelineError> {
        self.record_periods(cat)?;
        let mut reserved = self
            .reserved
            .lock()
            .map_err(|_| PipelineError::Invalid("budget state poisoned".into()))?;
        // Report the first (strictest) problem; the week is checked before the month.
        for p in self.periods(cat) {
            let Some(cap) = self.caps.of(p.kind) else {
                continue;
            };
            if needed_usd > cap + EPS {
                return Ok(Gate::TooLarge {
                    kind: p.kind,
                    needed_usd,
                    cap_usd: cap,
                });
            }
            let spent = cat.spend_between(p.starts_at, p.ends_at)?.cost_usd;
            let committed = spent + *reserved;
            if committed + needed_usd > cap + EPS || committed >= cap - EPS {
                return Ok(Gate::Stop(Blocked {
                    kind: p.kind,
                    cap_usd: cap,
                    spent_usd: spent,
                    period_start: p.start,
                    resets_at: p.ends_at,
                }));
            }
        }
        *reserved += needed_usd;
        drop(reserved);
        Ok(Gate::Go(Reservation {
            gate: self,
            amount: needed_usd,
        }))
    }
}

// ---------------------------------------------------------------------------
// Reporting (sb stats, sb budget, sb doctor)
// ---------------------------------------------------------------------------

/// A period with its cap and spend.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PeriodStatus {
    pub kind: PeriodKind,
    pub period_start: NaiveDate,
    /// The local date the next period starts on (the last day is the day before).
    pub ends_on: NaiveDate,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    /// `None` = the cap is disabled (or was, when the period was recorded).
    pub cap_usd: Option<f64>,
    pub spent_usd: f64,
    pub calls: u64,
    pub unpriced_calls: u64,
    /// When a cap first stopped a run in this period.
    pub stopped_at: Option<DateTime<Utc>>,
    /// The moment the period ends and the next one starts.
    pub resets_at: DateTime<Utc>,
}

impl PeriodStatus {
    /// Share of the cap used, in percent; `None` without a cap.
    pub fn used_percent(&self) -> Option<f64> {
        self.cap_usd
            .filter(|c| *c > 0.0)
            .map(|c| self.spent_usd / c * 100.0)
    }
}

/// The current week and month.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CurrentBudget {
    pub week: PeriodStatus,
    pub month: PeriodStatus,
}

/// Total spend since the ledger began.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TotalSpend {
    pub spent_usd: f64,
    pub calls: u64,
    pub unpriced_calls: u64,
    pub since: Option<DateTime<Utc>>,
}

/// The full `sb budget` report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BudgetReport {
    pub current: CurrentBudget,
    pub weeks: Vec<PeriodStatus>,
    pub months: Vec<PeriodStatus>,
    pub total: TotalSpend,
    pub by_model: Vec<ModelSpend>,
}

fn status_of(
    cat: &Catalog,
    period: &Period,
    cap_usd: Option<f64>,
    stopped_at: Option<DateTime<Utc>>,
) -> Result<PeriodStatus, PipelineError> {
    let s: Spend = cat.spend_between(period.starts_at, period.ends_at)?;
    Ok(PeriodStatus {
        kind: period.kind,
        period_start: period.start,
        ends_on: next_start(period.kind, period.start),
        starts_at: period.starts_at,
        ends_at: period.ends_at,
        cap_usd,
        spent_usd: s.cost_usd,
        calls: s.calls,
        unpriced_calls: s.unpriced_calls,
        stopped_at,
        resets_at: period.ends_at,
    })
}

fn status_of_row(cat: &Catalog, row: &PeriodRow) -> Result<PeriodStatus, PipelineError> {
    // The stored boundaries are authoritative: a later change of time zone must
    // not move a past period.
    let period = Period {
        kind: row.kind,
        start: row.period_start,
        starts_at: row.starts_at,
        ends_at: row.ends_at,
    };
    status_of(cat, &period, row.cap_usd, row.stopped_at)
}

/// The current week and month. Read-only: it never writes a snapshot, so the cap
/// shown for a period the tool has not evaluated yet is the cap in force now.
pub fn status(cat: &Catalog, policy: &SummaryPolicy) -> Result<CurrentBudget, PipelineError> {
    let gate = BudgetGate::from_policy(policy);
    let [week, month] = gate.periods(cat);
    let one = |p: &Period| -> Result<PeriodStatus, PipelineError> {
        match cat.period_row(p.kind, p.start)? {
            // A recorded period keeps its boundaries; the cap shown is today's,
            // because the current period's cap is whatever is set now.
            Some(row) => {
                let mut s = status_of_row(cat, &row)?;
                s.cap_usd = gate.caps().of(p.kind);
                Ok(s)
            }
            None => status_of(cat, p, gate.caps().of(p.kind), None),
        }
    };
    Ok(CurrentBudget {
        week: one(&week)?,
        month: one(&month)?,
    })
}

/// Current periods, history, total and breakdown by model. Read-only.
pub fn report(
    cat: &Catalog,
    policy: &SummaryPolicy,
    weeks: u32,
    months: u32,
) -> Result<BudgetReport, PipelineError> {
    let current = status(cat, policy)?;
    let history = |kind: PeriodKind, n: u32| -> Result<Vec<PeriodStatus>, PipelineError> {
        cat.period_rows(kind, n)?
            .iter()
            .map(|r| status_of_row(cat, r))
            .collect()
    };
    let (total, since) = cat.spend_total()?;
    Ok(BudgetReport {
        current,
        weeks: history(PeriodKind::Week, weeks)?,
        months: history(PeriodKind::Month, months)?,
        total: TotalSpend {
            spent_usd: total.cost_usd,
            calls: total.calls,
            unpriced_calls: total.unpriced_calls,
            since,
        },
        by_model: cat.spend_by_model()?,
    })
}

/// The period before `p`, for tests and callers that walk back in time.
pub fn previous(p: &Period, tz: Tz) -> Period {
    period_before(p, tz)
}

#[cfg(test)]
mod tests {
    use super::*;
    use second_brain_kernel::clock::FixedClock;
    use second_brain_kernel::util::parse_ts;
    use second_brain_kernel::{Generator, GeneratorKind, Usage};
    use second_brain_store::{Home, NewUsage, UsageOutcome};
    use std::sync::Arc;

    fn at(s: &str) -> DateTime<Utc> {
        parse_ts(s).unwrap()
    }

    fn catalog(now: &str) -> (tempfile::TempDir, Catalog, Arc<FixedClock>) {
        let dir = tempfile::tempdir().unwrap();
        let mut cat = Catalog::create(&Home::new(dir.path().join("home"))).unwrap();
        let clock = Arc::new(FixedClock::new(at(now)));
        cat.set_clock(clock.clone());
        (dir, cat, clock)
    }

    fn spend(cat: &Catalog, usd: f64) {
        cat.record_usage(&NewUsage {
            run_id: None,
            entry_id: None,
            profile: "p".into(),
            generator: Generator {
                kind: GeneratorKind::LlmApi,
                provider: "anthropic".into(),
                model: "m".into(),
                prompt_version: None,
            },
            usage: Usage {
                calls: 1,
                ..Default::default()
            },
            cost_usd: Some(usd),
            outcome: UsageOutcome::Ok,
        })
        .unwrap();
    }

    fn gate(weekly: Option<f64>, monthly: Option<f64>) -> BudgetGate {
        BudgetGate::new(
            Caps {
                weekly_usd: weekly,
                monthly_usd: monthly,
            },
            Tz::UTC,
        )
    }

    #[test]
    fn allows_until_the_next_unit_would_not_fit() {
        let (_d, cat, _c) = catalog("2026-10-07T10:00:00Z");
        let g = gate(Some(2.0), Some(10.0));
        spend(&cat, 1.5);
        assert!(matches!(g.try_reserve(&cat, 0.4).unwrap(), Gate::Go(_)));
        // 1.5 spent + 0.6 needed > 2.0
        match g.try_reserve(&cat, 0.6).unwrap() {
            Gate::Stop(b) => {
                assert_eq!(b.kind, PeriodKind::Week);
                assert_eq!(b.stop_detail(), "budget.weekly");
                assert_eq!(b.resets_at, at("2026-10-12T00:00:00Z"));
            }
            other => panic!("expected a stop, got {other:?}"),
        }
    }

    #[test]
    fn in_flight_reservations_count_and_are_released_on_drop() {
        let (_d, cat, _c) = catalog("2026-10-07T10:00:00Z");
        let g = gate(Some(1.0), None);
        let a = g.try_reserve(&cat, 0.6).unwrap();
        assert!(matches!(a, Gate::Go(_)));
        // 0.6 in flight + 0.6 > 1.0
        assert!(matches!(g.try_reserve(&cat, 0.6).unwrap(), Gate::Stop(_)));
        drop(a);
        assert!(matches!(g.try_reserve(&cat, 0.6).unwrap(), Gate::Go(_)));
    }

    #[test]
    fn many_in_flight_units_never_exceed_the_cap_together() {
        let (_d, cat, _c) = catalog("2026-10-07T10:00:00Z");
        let g = gate(Some(1.0), None);
        // Eight units of $0.30 are asked for while none has finished: only three
        // fit ($0.90), because each reservation counts against the next.
        let held: Vec<_> = (0..8)
            .filter_map(|_| match g.try_reserve(&cat, 0.3).unwrap() {
                Gate::Go(r) => Some(r),
                _ => None,
            })
            .collect();
        assert_eq!(held.len(), 3);
    }

    #[test]
    fn cap_settings_are_read_safely() {
        let (_d, cat, _c) = catalog("2026-10-07T10:00:00Z");
        let caps = |cat: &Catalog| {
            let p = SummaryPolicy::load(cat).unwrap();
            (p.weekly_cap_usd, p.monthly_cap_usd)
        };
        // Seeded defaults.
        assert_eq!(caps(&cat), (Some(2.0), Some(10.0)));
        // Removed rows fall back to the defaults.
        cat.unset_setting("summary.budget.weekly_usd").unwrap();
        cat.unset_setting("summary.budget.monthly_usd").unwrap();
        assert_eq!(caps(&cat), (Some(2.0), Some(10.0)));
        // 0 and null disable a cap.
        cat.set_setting("summary.budget.weekly_usd", &serde_json::json!(0))
            .unwrap();
        cat.set_setting("summary.budget.monthly_usd", &serde_json::Value::Null)
            .unwrap();
        assert_eq!(caps(&cat), (None, None));
        // A damaged value never turns the guard off.
        cat.set_setting("summary.budget.weekly_usd", &serde_json::json!("lots"))
            .unwrap();
        cat.set_setting("summary.budget.monthly_usd", &serde_json::json!(-5))
            .unwrap();
        assert_eq!(caps(&cat), (Some(2.0), Some(10.0)));
        cat.set_setting("summary.budget.weekly_usd", &serde_json::json!(3.25))
            .unwrap();
        assert_eq!(caps(&cat).0, Some(3.25));
    }

    #[test]
    fn the_monthly_cap_binds_when_the_week_has_room() {
        let (_d, cat, clock) = catalog("2026-10-01T10:00:00Z");
        let g = gate(Some(2.0), Some(3.0));
        spend(&cat, 2.0);
        // New week, same month: the weekly cap has room, the monthly does not.
        clock.set(at("2026-10-12T10:00:00Z"));
        spend(&cat, 0.0);
        clock.set(at("2026-10-13T10:00:00Z"));
        assert!(matches!(g.try_reserve(&cat, 0.5).unwrap(), Gate::Go(_)));
        match g.try_reserve(&cat, 1.5).unwrap() {
            Gate::Stop(b) => assert_eq!(b.kind, PeriodKind::Month),
            other => panic!("expected a monthly stop, got {other:?}"),
        }
    }

    #[test]
    fn the_week_boundary_unblocks() {
        let (_d, cat, clock) = catalog("2026-10-11T22:00:00Z"); // Sunday
        let g = gate(Some(1.0), Some(100.0));
        spend(&cat, 1.0);
        assert!(matches!(g.try_reserve(&cat, 0.1).unwrap(), Gate::Stop(_)));
        clock.set(at("2026-10-12T00:00:00Z")); // Monday
        assert!(matches!(g.try_reserve(&cat, 0.1).unwrap(), Gate::Go(_)));
    }

    #[test]
    fn disabled_caps_never_block() {
        let (_d, cat, _c) = catalog("2026-10-07T10:00:00Z");
        spend(&cat, 1_000.0);
        let g = gate(None, None);
        assert!(!g.enabled());
        assert!(matches!(g.try_reserve(&cat, 50.0).unwrap(), Gate::Go(_)));
        assert_eq!(g.remaining(&cat).unwrap(), None);
        // A single disabled cap leaves the other in force.
        let g = gate(None, Some(10.0));
        assert!(
            matches!(g.try_reserve(&cat, 1.0).unwrap(), Gate::Stop(b) if b.kind == PeriodKind::Month)
        );
    }

    #[test]
    fn a_unit_larger_than_the_whole_cap_is_skipped_not_blocking() {
        let (_d, cat, _c) = catalog("2026-10-07T10:00:00Z");
        let g = gate(Some(2.0), Some(10.0));
        assert!(matches!(
            g.try_reserve(&cat, 2.5).unwrap(),
            Gate::TooLarge {
                kind: PeriodKind::Week,
                ..
            }
        ));
    }

    #[test]
    fn an_exhausted_budget_stops_even_a_zero_estimate() {
        let (_d, cat, _c) = catalog("2026-10-07T10:00:00Z");
        let g = gate(Some(1.0), None);
        spend(&cat, 1.0);
        assert!(matches!(g.try_reserve(&cat, 0.0).unwrap(), Gate::Stop(_)));
    }

    #[test]
    fn remaining_is_the_strictest_cap() {
        let (_d, cat, _c) = catalog("2026-10-07T10:00:00Z");
        spend(&cat, 0.5);
        let g = gate(Some(2.0), Some(1.0));
        assert!((g.remaining(&cat).unwrap().unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn evaluation_records_the_period_snapshots_and_the_stop() {
        let (_d, cat, _c) = catalog("2026-10-07T10:00:00Z");
        let g = gate(Some(1.0), Some(5.0));
        spend(&cat, 1.0);
        let Gate::Stop(b) = g.try_reserve(&cat, 0.1).unwrap() else {
            panic!("expected a stop");
        };
        g.record_stop(&cat, &b).unwrap();
        let week = cat
            .period_row(
                PeriodKind::Week,
                NaiveDate::from_ymd_opt(2026, 10, 5).unwrap(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(week.cap_usd, Some(1.0));
        assert!(week.stopped_at.is_some());
        let month = cat
            .period_row(
                PeriodKind::Month,
                NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(month.cap_usd, Some(5.0));
        assert!(month.stopped_at.is_none());
    }

    #[test]
    fn report_shows_history_with_the_cap_of_that_time() {
        let (_d, cat, clock) = catalog("2026-09-29T10:00:00Z");
        let mut policy = SummaryPolicy::load(&cat).unwrap();
        policy.budget_timezone = "UTC".into();
        policy.weekly_cap_usd = Some(2.0);
        policy.monthly_cap_usd = Some(10.0);
        let g = BudgetGate::from_policy(&policy);
        spend(&cat, 2.0);
        let Gate::Stop(b) = g.try_reserve(&cat, 0.5).unwrap() else {
            panic!("expected a stop");
        };
        g.record_stop(&cat, &b).unwrap();

        // The next week, with a different cap.
        clock.set(at("2026-10-06T10:00:00Z"));
        policy.weekly_cap_usd = Some(4.0);
        let g = BudgetGate::from_policy(&policy);
        spend(&cat, 0.75);
        assert!(matches!(g.try_reserve(&cat, 0.1).unwrap(), Gate::Go(_)));

        let r = report(&cat, &policy, 8, 6).unwrap();
        assert_eq!(r.weeks.len(), 2);
        // Newest first.
        assert_eq!(
            r.weeks[0].period_start,
            NaiveDate::from_ymd_opt(2026, 10, 5).unwrap()
        );
        assert_eq!(r.weeks[0].cap_usd, Some(4.0));
        assert!((r.weeks[0].spent_usd - 0.75).abs() < 1e-9);
        assert!(r.weeks[0].stopped_at.is_none());
        assert_eq!(
            r.weeks[1].cap_usd,
            Some(2.0),
            "the cap of that week, not today's"
        );
        assert!((r.weeks[1].spent_usd - 2.0).abs() < 1e-9);
        assert!(r.weeks[1].stopped_at.is_some());
        assert_eq!(r.months.len(), 2);
        assert!((r.total.spent_usd - 2.75).abs() < 1e-9);
        assert_eq!(r.total.since, Some(at("2026-09-29T10:00:00Z")));
        assert_eq!(r.by_model.len(), 1);
        assert!((r.current.week.used_percent().unwrap() - 18.75).abs() < 1e-9);
    }

    #[test]
    fn status_and_report_do_not_write() {
        let (_d, cat, _c) = catalog("2026-10-07T10:00:00Z");
        let policy = SummaryPolicy::load(&cat).unwrap();
        let _ = status(&cat, &policy).unwrap();
        let _ = report(&cat, &policy, 8, 6).unwrap();
        assert!(cat.period_rows(PeriodKind::Week, 10).unwrap().is_empty());
        assert!(cat.period_rows(PeriodKind::Month, 10).unwrap().is_empty());
        assert_eq!(
            cat.spend_total().unwrap().0.calls,
            0,
            "an empty ledger works"
        );
    }

    #[test]
    fn period_history_keeps_boundaries_after_a_time_zone_change() {
        let (_d, cat, _c) = catalog("2026-10-07T10:00:00Z");
        let mut policy = SummaryPolicy::load(&cat).unwrap();
        policy.budget_timezone = "Asia/Tokyo".into();
        let g = BudgetGate::from_policy(&policy);
        g.record_periods(&cat).unwrap();
        let before = report(&cat, &policy, 8, 6).unwrap().weeks[0].clone();
        policy.budget_timezone = "America/New_York".into();
        let after = report(&cat, &policy, 8, 6).unwrap().weeks[0].clone();
        assert_eq!(before.starts_at, after.starts_at);
        assert_eq!(before.ends_at, after.ends_at);
    }
}
