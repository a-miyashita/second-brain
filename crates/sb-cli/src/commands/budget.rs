//! `sb budget`: summarization spend against the weekly and monthly budget
//! (ADR-0013, summarization.md).

use sb_core::budget::{Tz, parse_tz};
use sb_pipeline::budget::{self, BudgetReport, PeriodStatus};
use sb_pipeline::policy::SummaryPolicy;

use crate::Ctx;
use crate::cli::BudgetArgs;
use crate::util::exit;

/// Money with two decimals; four below one cent so small amounts are visible.
pub(crate) fn usd(v: f64) -> String {
    if v > 0.0 && v < 0.01 {
        format!("${v:.4}")
    } else {
        format!("${v:.2}")
    }
}

fn cap(c: Option<f64>) -> String {
    c.map_or_else(|| "no cap".to_string(), usd)
}

fn percent(p: &PeriodStatus) -> String {
    p.used_percent()
        .map_or_else(|| "-".to_string(), |v| format!("{v:.0}%"))
}

fn tokens(n: u64) -> String {
    match n {
        1_000_000.. => format!("{:.1}M", n as f64 / 1e6),
        1_000.. => format!("{:.1}k", n as f64 / 1e3),
        _ => n.to_string(),
    }
}

/// The last day of a period (its end date is exclusive).
fn last_day(p: &PeriodStatus) -> chrono::NaiveDate {
    p.ends_on.pred_opt().unwrap_or(p.ends_on)
}

/// The block shared by `sb stats` and `sb budget`.
pub(crate) fn print_current(week: &PeriodStatus, month: &PeriodStatus, reset_hint: bool) {
    for (label, p) in [("this week", week), ("this month", month)] {
        let resets = if reset_hint {
            format!("   resets {}", p.ends_on.format("%Y-%m-%d (%a)"))
        } else {
            String::new()
        };
        println!(
            "  {label:<11} {:>8} / {:<9}{resets}",
            usd(p.spent_usd),
            cap(p.cap_usd)
        );
    }
}

fn print_history(title: &str, rows: &[PeriodStatus], tz: &Tz) {
    println!("\n{title}");
    if rows.is_empty() {
        println!("  (none recorded yet)");
        return;
    }
    println!(
        "  {:<10}  {:>9}  {:>9}  {:>5}  {:>6}  stopped",
        "start", "cap", "spent", "used", "calls"
    );
    for p in rows {
        let stopped = p.stopped_at.map_or_else(
            || "-".to_string(),
            |t| t.with_timezone(tz).format("%Y-%m-%d %H:%M").to_string(),
        );
        println!(
            "  {:<10}  {:>9}  {:>9}  {:>5}  {:>6}  {}",
            p.period_start.format("%Y-%m-%d"),
            p.cap_usd.map_or_else(|| "-".to_string(), usd),
            usd(p.spent_usd),
            percent(p),
            p.calls,
            stopped
        );
    }
}

fn print_report(r: &BudgetReport, tz: &Tz, by_model: bool) {
    println!("Summarization spend (estimated by second-brain, not an invoice)\n");
    println!("Current");
    for (label, p) in [("week", &r.current.week), ("month", &r.current.month)] {
        println!(
            "  {label:<6} {} .. {}   {} / {}   {}",
            p.period_start.format("%Y-%m-%d"),
            last_day(p).format("%Y-%m-%d"),
            usd(p.spent_usd),
            cap(p.cap_usd),
            percent(p)
        );
    }
    match r.total.since {
        Some(since) => println!(
            "Total since {}: {} in {} calls ({} unpriced)",
            since.with_timezone(tz).format("%Y-%m-%d"),
            usd(r.total.spent_usd),
            r.total.calls,
            r.total.unpriced_calls
        ),
        None => println!("Nothing has been spent yet: no paid summarization is recorded."),
    }
    print_history("Weeks", &r.weeks, tz);
    print_history("Months", &r.months, tz);
    if by_model {
        println!("\nBy model (all time)");
        if r.by_model.is_empty() {
            println!("  (none recorded yet)");
        }
        for m in &r.by_model {
            println!(
                "  {:<28} {:>9}  {:>6} calls  {} in / {} out tokens",
                m.model,
                usd(m.spend.cost_usd),
                m.spend.calls,
                tokens(m.spend.input_tokens),
                tokens(m.spend.output_tokens)
            );
        }
    }
}

pub fn run(ctx: &Ctx, a: BudgetArgs) -> anyhow::Result<i32> {
    let cat = ctx.catalog()?;
    let policy = SummaryPolicy::load(&cat)?;
    let report = budget::report(&cat, &policy, a.weeks, a.months)?;
    if ctx.json {
        ctx.out_json("sb.budget/v1", serde_json::to_value(&report)?);
        return Ok(exit::OK);
    }
    let tz = parse_tz(&policy.budget_timezone);
    print_report(&report, &tz, a.by_model);
    Ok(exit::OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_shows_small_amounts() {
        assert_eq!(usd(0.0), "$0.00");
        assert_eq!(usd(0.004), "$0.0040");
        assert_eq!(usd(2.0), "$2.00");
        assert_eq!(usd(12.345), "$12.35");
        assert_eq!(cap(None), "no cap");
        assert_eq!(cap(Some(10.0)), "$10.00");
    }

    #[test]
    fn token_counts_are_compact() {
        assert_eq!(tokens(999), "999");
        assert_eq!(tokens(1_500), "1.5k");
        assert_eq!(tokens(1_900_000), "1.9M");
    }
}
