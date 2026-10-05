//! The summarization usage ledger and budget period snapshots (ADR-0013,
//! data-model.md).
//!
//! The ledger (`llm_usage`) is the only copy of what was spent. The snapshots
//! (`budget_periods`) record the budget side: the boundaries of each period and
//! the cap in effect.

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{Connection, params};
use sb_core::budget::{Period, PeriodKind};
use sb_core::util::{parse_ts, ts};
use sb_core::{Generator, Usage};
use serde::Serialize;

use crate::catalog::{Catalog, OptionalExt, opt_ts, parse_col, req_ts};
use crate::error::Result;

/// Whether an attempt produced a usable summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageOutcome {
    Ok,
    /// Billed, but no usable summary came out of it.
    Failed,
}

impl UsageOutcome {
    fn as_str(self) -> &'static str {
        match self {
            UsageOutcome::Ok => "ok",
            UsageOutcome::Failed => "failed",
        }
    }
}

/// A ledger row to append.
#[derive(Debug, Clone, PartialEq)]
pub struct NewUsage {
    pub run_id: Option<i64>,
    pub entry_id: Option<i64>,
    pub profile: String,
    pub generator: Generator,
    pub usage: Usage,
    /// `None` when no price is known; `Some(0.0)` for local models.
    pub cost_usd: Option<f64>,
    pub outcome: UsageOutcome,
}

/// Append a ledger row. Used inside the summary-commit transaction.
pub(crate) fn insert_usage(conn: &Connection, now: &str, u: &NewUsage) -> Result<()> {
    conn.execute(
        "INSERT INTO llm_usage(at, run_id, entry_id, profile, generator_kind, provider, model,
                               input_tokens, output_tokens, calls, cost_usd, outcome)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            now,
            u.run_id,
            u.entry_id,
            u.profile,
            u.generator.kind.as_str(),
            u.generator.provider,
            u.generator.model,
            u.usage.input_tokens as i64,
            u.usage.output_tokens as i64,
            i64::from(u.usage.calls),
            u.cost_usd,
            u.outcome.as_str(),
        ],
    )?;
    Ok(())
}

/// Totals over a set of ledger rows.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Spend {
    /// Sum of the known costs. Rows with an unknown cost add nothing.
    #[serde(rename = "spent_usd")]
    pub cost_usd: f64,
    /// LLM calls.
    pub calls: u64,
    /// LLM calls whose cost is unknown.
    pub unpriced_calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Spend of one model, over all time.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelSpend {
    pub model: String,
    pub provider: String,
    #[serde(flatten)]
    pub spend: Spend,
}

/// A stored budget period snapshot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PeriodRow {
    pub kind: PeriodKind,
    pub period_start: NaiveDate,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    /// The cap at the last evaluation; `None` = disabled.
    pub cap_usd: Option<f64>,
    pub cap_updated_at: DateTime<Utc>,
    /// When a cap first stopped a run in this period.
    pub stopped_at: Option<DateTime<Utc>>,
}

const SPEND_COLS: &str = "IFNULL(SUM(cost_usd), 0.0), IFNULL(SUM(calls), 0),
     IFNULL(SUM(CASE WHEN cost_usd IS NULL THEN calls ELSE 0 END), 0),
     IFNULL(SUM(input_tokens), 0), IFNULL(SUM(output_tokens), 0)";

fn spend_from_row(r: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<Spend> {
    Ok(Spend {
        cost_usd: r.get(at)?,
        calls: r.get::<_, i64>(at + 1)? as u64,
        unpriced_calls: r.get::<_, i64>(at + 2)? as u64,
        input_tokens: r.get::<_, i64>(at + 3)? as u64,
        output_tokens: r.get::<_, i64>(at + 4)? as u64,
    })
}

fn period_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PeriodRow> {
    let start: String = r.get(1)?;
    Ok(PeriodRow {
        kind: parse_col(r.get(0)?, 0)?,
        period_start: NaiveDate::parse_from_str(&start, "%Y-%m-%d").map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?,
        starts_at: req_ts(r.get(2)?),
        ends_at: req_ts(r.get(3)?),
        cap_usd: r.get(4)?,
        cap_updated_at: req_ts(r.get(5)?),
        stopped_at: opt_ts(r.get(6)?),
    })
}

const PERIOD_COLS: &str =
    "kind, period_start, starts_at, ends_at, cap_usd, cap_updated_at, stopped_at";

impl Catalog {
    /// Append a ledger row on its own (attempts that failed after being billed).
    pub fn record_usage(&self, u: &NewUsage) -> Result<()> {
        insert_usage(&self.conn, &self.now_ts(), u)
    }

    /// Spend over `[from, to)`.
    pub fn spend_between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Spend> {
        Ok(self.conn.query_row(
            &format!("SELECT {SPEND_COLS} FROM llm_usage WHERE at >= ?1 AND at < ?2"),
            params![ts(from), ts(to)],
            |r| spend_from_row(r, 0),
        )?)
    }

    /// Spend over the whole ledger, and the time of its first row.
    pub fn spend_total(&self) -> Result<(Spend, Option<DateTime<Utc>>)> {
        let spend =
            self.conn
                .query_row(&format!("SELECT {SPEND_COLS} FROM llm_usage"), [], |r| {
                    spend_from_row(r, 0)
                })?;
        let first: Option<String> =
            self.conn
                .query_row("SELECT MIN(at) FROM llm_usage", [], |r| r.get(0))?;
        Ok((spend, first.as_deref().and_then(parse_ts)))
    }

    /// Spend per model over the whole ledger, largest first.
    pub fn spend_by_model(&self) -> Result<Vec<ModelSpend>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT model, provider, {SPEND_COLS} FROM llm_usage
             GROUP BY model, provider
             ORDER BY SUM(cost_usd) IS NULL, SUM(cost_usd) DESC, SUM(calls) DESC, model"
        ))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ModelSpend {
                    model: r.get(0)?,
                    provider: r.get(1)?,
                    spend: spend_from_row(r, 2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Create the snapshot of a period, or update its cap. The boundaries are
    /// written once and never recomputed. Writes nothing when nothing changed.
    pub fn upsert_period(&self, p: &Period, cap_usd: Option<f64>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO budget_periods(kind, period_start, starts_at, ends_at, cap_usd, cap_updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(kind, period_start) DO UPDATE
               SET cap_usd = excluded.cap_usd, cap_updated_at = excluded.cap_updated_at
               WHERE budget_periods.cap_usd IS NOT excluded.cap_usd",
            params![
                p.kind.as_str(),
                p.start.format("%Y-%m-%d").to_string(),
                ts(p.starts_at),
                ts(p.ends_at),
                cap_usd,
                self.now_ts(),
            ],
        )?;
        Ok(())
    }

    /// Note that a cap stopped a run in this period (the first time only).
    pub fn mark_period_stopped(&self, kind: PeriodKind, start: NaiveDate) -> Result<()> {
        self.conn.execute(
            "UPDATE budget_periods SET stopped_at = ?3
             WHERE kind = ?1 AND period_start = ?2 AND stopped_at IS NULL",
            params![
                kind.as_str(),
                start.format("%Y-%m-%d").to_string(),
                self.now_ts()
            ],
        )?;
        Ok(())
    }

    /// One period snapshot.
    pub fn period_row(&self, kind: PeriodKind, start: NaiveDate) -> Result<Option<PeriodRow>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {PERIOD_COLS} FROM budget_periods WHERE kind = ?1 AND period_start = ?2"
                ),
                params![kind.as_str(), start.format("%Y-%m-%d").to_string()],
                period_from_row,
            )
            .opt()
    }

    /// Period snapshots of a kind, newest first.
    pub fn period_rows(&self, kind: PeriodKind, limit: u32) -> Result<Vec<PeriodRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {PERIOD_COLS} FROM budget_periods WHERE kind = ?1
             ORDER BY period_start DESC LIMIT ?2"
        ))?;
        let rows = stmt
            .query_map(params![kind.as_str(), i64::from(limit)], period_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::test_util::temp_catalog;
    use chrono_tz::Tz;
    use sb_core::GeneratorKind;
    use sb_core::budget::period_containing;
    use sb_core::clock::FixedClock;
    use serde_json::json;
    use std::sync::Arc;

    fn gen_(model: &str, kind: GeneratorKind) -> Generator {
        Generator {
            kind,
            provider: "anthropic".into(),
            model: model.into(),
            prompt_version: None,
        }
    }

    fn row(model: &str, cost: Option<f64>, calls: u32) -> NewUsage {
        NewUsage {
            run_id: None,
            entry_id: None,
            profile: "p".into(),
            generator: gen_(model, GeneratorKind::LlmApi),
            usage: Usage {
                input_tokens: 100,
                output_tokens: 10,
                calls,
                ..Default::default()
            },
            cost_usd: cost,
            outcome: UsageOutcome::Ok,
        }
    }

    fn at(s: &str) -> DateTime<Utc> {
        parse_ts(s).unwrap()
    }

    #[test]
    fn fresh_home_has_the_budget_settings_as_stored_values() {
        let (_d, cat) = temp_catalog();
        assert_eq!(
            cat.setting_raw("summary.budget.weekly_usd").unwrap(),
            Some(json!(2.0))
        );
        assert_eq!(
            cat.setting_raw("summary.budget.monthly_usd").unwrap(),
            Some(json!(10.0))
        );
        let listed = cat.list_settings().unwrap();
        let weekly = listed
            .iter()
            .find(|i| i.key == "summary.budget.weekly_usd")
            .unwrap();
        assert!(!weekly.default, "seeded rows are stored values");
    }

    #[test]
    fn migration_keeps_values_the_user_already_set() {
        let dir = tempfile::tempdir().unwrap();
        let home = crate::Home::new(dir.path().join("home"));
        let cat = Catalog::create(&home).unwrap();
        cat.set_setting("summary.budget.weekly_usd", &json!(0))
            .unwrap();
        cat.set_setting("summary.budget.monthly_usd", &serde_json::Value::Null)
            .unwrap();
        // Simulate a home that has not had migration 2 yet, then migrate.
        cat.conn
            .execute_batch("DROP TABLE llm_usage; DROP TABLE budget_periods; DELETE FROM schema_migrations WHERE version = 2;")
            .unwrap();
        cat.migrate().unwrap();
        cat.migrate().unwrap();
        assert_eq!(cat.schema_version().unwrap(), crate::SCHEMA_VERSION);
        assert_eq!(
            cat.setting_raw("summary.budget.weekly_usd").unwrap(),
            Some(json!(0))
        );
        assert_eq!(
            cat.setting_raw("summary.budget.monthly_usd").unwrap(),
            Some(serde_json::Value::Null)
        );
    }

    #[test]
    fn spend_is_summed_per_period_and_unpriced_calls_are_counted() {
        let (_d, mut cat) = temp_catalog();
        let clock = Arc::new(FixedClock::new(at("2026-10-05T10:00:00Z")));
        cat.set_clock(clock.clone());
        cat.record_usage(&row("haiku", Some(0.25), 1)).unwrap();
        clock.set(at("2026-10-06T10:00:00Z"));
        cat.record_usage(&row("haiku", Some(0.5), 2)).unwrap();
        cat.record_usage(&row("cli-model", None, 3)).unwrap();
        clock.set(at("2026-10-12T00:00:00Z"));
        cat.record_usage(&row("haiku", Some(1.0), 1)).unwrap();

        let week = period_containing(PeriodKind::Week, at("2026-10-07T00:00:00Z"), Tz::UTC);
        let s = cat.spend_between(week.starts_at, week.ends_at).unwrap();
        assert!((s.cost_usd - 0.75).abs() < 1e-9);
        assert_eq!((s.calls, s.unpriced_calls), (6, 3));
        assert_eq!((s.input_tokens, s.output_tokens), (300, 30));
        // The end is exclusive: a row exactly at the boundary belongs to the next period.
        let next = period_containing(PeriodKind::Week, at("2026-10-12T00:00:00Z"), Tz::UTC);
        assert_eq!(next.starts_at, week.ends_at);
        assert!(
            (cat.spend_between(next.starts_at, next.ends_at)
                .unwrap()
                .cost_usd
                - 1.0)
                .abs()
                < 1e-9
        );

        let (total, since) = cat.spend_total().unwrap();
        assert!((total.cost_usd - 1.75).abs() < 1e-9);
        assert_eq!(since, Some(at("2026-10-05T10:00:00Z")));
        let by_model = cat.spend_by_model().unwrap();
        assert_eq!(by_model[0].model, "haiku");
        assert_eq!(by_model[0].spend.calls, 4);
        assert_eq!(by_model.last().unwrap().model, "cli-model");
    }

    #[test]
    fn empty_ledger_reports_zero() {
        let (_d, cat) = temp_catalog();
        let (s, since) = cat.spend_total().unwrap();
        assert_eq!(s, Spend::default());
        assert!(since.is_none());
        assert!(cat.spend_by_model().unwrap().is_empty());
    }

    #[test]
    fn period_snapshots_keep_boundaries_and_follow_the_cap() {
        let (_d, mut cat) = temp_catalog();
        let clock = Arc::new(FixedClock::new(at("2026-10-05T10:00:00Z")));
        cat.set_clock(clock.clone());
        let tokyo: Tz = chrono_tz::Asia::Tokyo;
        let p = period_containing(PeriodKind::Week, at("2026-10-05T10:00:00Z"), tokyo);
        cat.upsert_period(&p, Some(2.0)).unwrap();
        let first = cat.period_row(p.kind, p.start).unwrap().unwrap();
        assert_eq!(first.cap_usd, Some(2.0));
        assert_eq!(first.starts_at, p.starts_at);
        assert!(first.stopped_at.is_none());

        // Unchanged cap: no write (cap_updated_at stays).
        clock.set(at("2026-10-06T10:00:00Z"));
        cat.upsert_period(&p, Some(2.0)).unwrap();
        assert_eq!(
            cat.period_row(p.kind, p.start)
                .unwrap()
                .unwrap()
                .cap_updated_at,
            first.cap_updated_at
        );

        // A changed cap updates the snapshot, even to "disabled"; boundaries stay
        // even if a different time zone computes other ones.
        let utc_p = period_containing(PeriodKind::Week, at("2026-10-07T00:00:00Z"), Tz::UTC);
        assert_eq!(utc_p.start, p.start);
        assert_ne!(utc_p.starts_at, p.starts_at);
        cat.upsert_period(&utc_p, None).unwrap();
        let updated = cat.period_row(p.kind, p.start).unwrap().unwrap();
        assert_eq!(updated.cap_usd, None);
        assert_eq!(updated.cap_updated_at, at("2026-10-06T10:00:00Z"));
        assert_eq!(updated.starts_at, p.starts_at);
        assert_eq!(updated.ends_at, p.ends_at);

        // stopped_at is set once.
        cat.mark_period_stopped(p.kind, p.start).unwrap();
        clock.set(at("2026-10-07T10:00:00Z"));
        cat.mark_period_stopped(p.kind, p.start).unwrap();
        assert_eq!(
            cat.period_row(p.kind, p.start).unwrap().unwrap().stopped_at,
            Some(at("2026-10-06T10:00:00Z"))
        );
    }

    #[test]
    fn period_rows_are_newest_first_and_limited() {
        let (_d, cat) = temp_catalog();
        let mut p = period_containing(PeriodKind::Week, at("2026-10-07T00:00:00Z"), Tz::UTC);
        for _ in 0..5 {
            cat.upsert_period(&p, Some(2.0)).unwrap();
            p = sb_core::budget::period_before(&p, Tz::UTC);
        }
        cat.upsert_period(
            &period_containing(PeriodKind::Month, at("2026-10-07T00:00:00Z"), Tz::UTC),
            Some(10.0),
        )
        .unwrap();
        let weeks = cat.period_rows(PeriodKind::Week, 3).unwrap();
        assert_eq!(weeks.len(), 3);
        assert!(weeks[0].period_start > weeks[1].period_start);
        assert_eq!(cat.period_rows(PeriodKind::Month, 10).unwrap().len(), 1);
    }
}
