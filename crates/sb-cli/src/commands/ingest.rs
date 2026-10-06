//! `sb sync`, `refetch`, `reextract`, `summarize`, `resummarize`, `import`.

use std::time::Instant;

use sb_core::{RawStatus, RunStatus, SyncMode};
use sb_pipeline::summarize::{Estimate, SummarizeOptions, SummarizeReport, Target};
use sb_pipeline::sync::SyncOptions;
use sb_pipeline::{Limits, RunStats, Stop};
use serde_json::json;

use crate::Ctx;
use crate::cli::{
    ImportArgs, IngestArgs, LimitArgs, ReextractArgs, RefetchArgs, ResummarizeArgs, SummarizeArgs,
    SyncArgs,
};
use crate::util::{self, exit, filter_from, parse_date, parse_duration, parse_source_kinds, usage};

fn limits(a: &LimitArgs) -> anyhow::Result<Limits> {
    Ok(Limits {
        max_summaries: a.max_summaries,
        max_cost_usd: a.max_cost,
        deadline: a
            .time_limit
            .as_deref()
            .map(parse_duration)
            .transpose()?
            .map(|d| Instant::now() + d),
    })
}

fn time_limit(t: &Option<String>) -> anyhow::Result<Limits> {
    limits(&LimitArgs {
        time_limit: t.clone(),
        ..Default::default()
    })
}

/// Exit code for a stop reason (limits exit 0; interruption 130).
fn stop_exit(stop: &Option<Stop>, problems: bool) -> i32 {
    match stop {
        Some(Stop::Cancelled) => exit::INTERRUPTED,
        _ if problems => exit::PROBLEMS,
        _ => exit::OK,
    }
}

fn print_estimate(e: &Estimate) {
    eprintln!(
        "Estimate: {} entries, {} LLM calls, ~{} input / ~{} output tokens",
        e.entries, e.calls, e.input_tokens, e.output_tokens
    );
    match e.cost_usd {
        Some(c) => {
            eprintln!("Estimated cost: ~${c:.2} (CLI and local providers are counted as $0)")
        }
        None => eprintln!(
            "Estimated cost: unknown (no price for {}; set llm.prices)",
            e.unpriced_models.join(", ")
        ),
    }
    if let Some(b) = &e.budget {
        if b.fits {
            eprintln!(
                "Budget: {} left under the strictest cap; the estimate fits.",
                super::budget::usd(b.remaining_usd)
            );
        } else {
            eprintln!(
                "Budget: {} left under the strictest cap; only {} of {} paid summaries fit, then the run stops (see `sb budget`).",
                super::budget::usd(b.remaining_usd),
                b.entries_that_fit,
                b.paid_entries
            );
        }
    }
}

fn print_summary_stats(r: &SummarizeReport) {
    let s = &r.stats;
    eprintln!(
        "Summaries: {} done, {} failed, {} skipped; ~${:.4} ({} in / {} out tokens){}",
        s.summarized,
        s.failed,
        s.skipped,
        s.cost_usd,
        s.input_tokens,
        s.output_tokens,
        if s.unpriced > 0 {
            format!("; {} without a known price", s.unpriced)
        } else {
            String::new()
        }
    );
}

fn stop_hint(stop: &Option<Stop>, command: &str) {
    match stop {
        Some(Stop::Cancelled) => {
            eprintln!("Interrupted. Committed work is kept; run `{command}` again to continue.")
        }
        Some(Stop::Limit(l)) if l.starts_with("budget.") => {
            let (which, key) = if l == "budget.monthly" {
                ("monthly", "monthly")
            } else {
                ("weekly", "weekly")
            };
            eprintln!(
                "Stopped: the {which} summarization budget is used up (see `sb budget`). Run `{command}` again after it resets, or raise it with `sb config set summary.budget.{key}_usd <amount>`."
            );
        }
        Some(Stop::Limit(l)) => eprintln!("Stopped by {l}. Run `{command}` again to continue."),
        None => {}
    }
}

pub async fn sync(ctx: &Ctx, a: SyncArgs) -> anyhow::Result<i32> {
    let p = ctx.pipeline()?;
    let opts = SyncOptions {
        accounts: a.accounts.clone(),
        sources: parse_source_kinds(&a.sources)?,
        mode: if a.deep {
            SyncMode::Deep
        } else {
            SyncMode::Normal
        },
        since: a
            .since
            .as_deref()
            .map(|s| parse_date(s, false))
            .transpose()?,
        no_summary: a.no_summary,
        limits: limits(&a.limits)?,
        dry_run: a.dry_run,
        estimate: a.estimate,
    };
    let r = p.sync(&opts).await?;
    if let Some(dr) = &r.dry_run {
        if ctx.json {
            ctx.out_json("sb.sync/v1", json!({"dry_run": dr}));
        } else {
            for acc in &dr.accounts {
                match &acc.skipped_reason {
                    Some(why) => println!("{} ({}): skipped: {why}", acc.id, acc.kind),
                    None => println!(
                        "{} ({}): {} [queued: {}]",
                        acc.id,
                        acc.kind,
                        acc.sources.join(", "),
                        acc.queued
                    ),
                }
            }
            println!("Pending summaries: {}", dr.pending_summaries);
        }
        return Ok(exit::OK);
    }
    if a.estimate {
        let est = r
            .summarize
            .as_ref()
            .and_then(|s| s.estimate.clone())
            .unwrap_or_default();
        if ctx.json {
            ctx.out_json("sb.sync/v1", json!({"estimate": est}));
        } else {
            print_estimate(&est);
        }
        return Ok(exit::OK);
    }
    let status = r.status.unwrap_or(RunStatus::Ok);
    if ctx.json {
        ctx.out_json(
            "sb.sync/v1",
            json!({"run_id": r.run_id, "status": status, "stats": r.stats}),
        );
    } else {
        print_run_stats(&r.stats);
        if let Some(s) = &r.summarize {
            print_summary_stats(s);
        }
        eprintln!(
            "Remaining: {} queued fetches, {} pending summaries. Status: {status}",
            r.stats.queue_remaining, r.stats.pending_summaries
        );
        for e in r.stats.errors.iter().take(10) {
            eprintln!("  problem: {e}");
        }
        stop_hint(&r.stats.stop, "sb sync");
    }
    Ok(match status {
        RunStatus::Interrupted => exit::INTERRUPTED,
        RunStatus::Partial => exit::PROBLEMS,
        RunStatus::Failed => exit::FAILURE,
        _ => exit::OK,
    })
}

fn print_run_stats(s: &RunStats) {
    for (k, v) in &s.sources {
        eprintln!(
            "{k}: {} new, {} updated, {} unchanged, {} not applicable, {} failed",
            v.new, v.updated, v.unchanged, v.not_applicable, v.failed
        );
    }
}

pub async fn refetch(ctx: &Ctx, a: RefetchArgs) -> anyhow::Result<i32> {
    let mut f = filter_from(&a.filters)?;
    if a.raw_missing {
        f.raw_status = vec![RawStatus::Missing, RawStatus::FetchFailed];
    }
    let p = ctx.pipeline()?;
    let r = p.refetch(&f, &time_limit(&a.time_limit)?).await?;
    if ctx.json {
        ctx.out_json("sb.refetch/v1", serde_json::to_value(&r)?);
    } else {
        eprintln!(
            "Refetch: {} selected, {} updated, {} unchanged, {} not applicable, {} failed, {} skipped",
            r.selected, r.updated, r.unchanged, r.not_applicable, r.failed, r.skipped
        );
        for e in r.errors.iter().take(10) {
            eprintln!("  problem: {e}");
        }
        stop_hint(&r.stop, "sb refetch");
    }
    Ok(stop_exit(&r.stop, r.failed > 0))
}

pub async fn reextract(ctx: &Ctx, a: ReextractArgs) -> anyhow::Result<i32> {
    let f = filter_from(&a.filters)?;
    let p = ctx.pipeline()?;
    let r = p.reextract(&f, &time_limit(&a.time_limit)?).await?;
    if ctx.json {
        ctx.out_json("sb.reextract/v1", serde_json::to_value(&r)?);
    } else {
        eprintln!(
            "Reextract: {} selected, {} updated, {} not applicable, {} failed, {} skipped (no raw data or no source)",
            r.selected, r.updated, r.not_applicable, r.failed, r.skipped
        );
        stop_hint(&r.stop, "sb reextract");
    }
    Ok(stop_exit(&r.stop, r.failed > 0))
}

pub async fn summarize(ctx: &Ctx, a: SummarizeArgs) -> anyhow::Result<i32> {
    let p = ctx.pipeline()?;
    let opts = SummarizeOptions {
        filter: filter_from(&a.filters)?,
        target: a
            .profile
            .clone()
            .map(Target::Profile)
            .unwrap_or(Target::Configured),
        limits: limits(&a.limits)?,
        retry_failed: a.retry_failed,
        force: false,
        estimate_only: a.estimate,
    };
    let r = p.summarize_pending(&opts).await?;
    if ctx.json {
        ctx.out_json("sb.summarize/v1", serde_json::to_value(&r)?);
    } else if let Some(e) = &r.estimate {
        print_estimate(e);
    } else {
        print_summary_stats(&r);
        eprintln!(
            "Remaining: {} pending or failed below the attempt limit.",
            r.stats.remaining
        );
        for e in r.errors.iter().take(10) {
            eprintln!("  problem: {e}");
        }
        stop_hint(&r.stop, "sb summarize");
    }
    Ok(stop_exit(
        &r.stop,
        r.stats.failed > 0 || !r.errors.is_empty(),
    ))
}

pub async fn resummarize(ctx: &Ctx, a: ResummarizeArgs) -> anyhow::Result<i32> {
    let target = match (&a.profile, a.native) {
        (Some(p), false) => Target::Profile(p.clone()),
        (None, true) => Target::Native,
        _ => return Err(usage("give --profile <name> or --native")),
    };
    let mut filter = filter_from(&a.filters)?;
    filter.where_model = a.where_model.clone();
    filter.where_provider = a.where_provider.clone();
    filter.limit = a.limit;
    let p = ctx.pipeline()?;
    let mut opts = SummarizeOptions {
        filter,
        target,
        limits: Limits {
            max_summaries: None,
            max_cost_usd: a.max_cost,
            deadline: a
                .time_limit
                .as_deref()
                .map(parse_duration)
                .transpose()?
                .map(|d| Instant::now() + d),
        },
        retry_failed: false,
        force: a.force,
        estimate_only: true,
    };
    let preview = p.resummarize(&opts).await?;
    if a.dry_run || a.estimate {
        if ctx.json {
            ctx.out_json(
                "sb.resummarize/v1",
                json!({"dry_run": true, "report": preview}),
            );
        } else {
            eprintln!(
                "Would resummarize {} entries ({} already at the target generator, {} without raw data; try `sb refetch` on the same filters).",
                preview.selected, preview.stats.already_current, preview.stats.no_raw
            );
            if let Some(e) = &preview.estimate {
                print_estimate(e);
            }
        }
        return Ok(exit::OK);
    }
    if preview.selected > 20 && !a.yes {
        if !util::interactive() {
            return Err(usage(format!(
                "{} entries would be resummarized; add --yes to confirm",
                preview.selected
            )));
        }
        if let Some(e) = &preview.estimate {
            print_estimate(e);
        }
        if !util::confirm(&format!("Resummarize {} entries?", preview.selected), false)? {
            return Ok(exit::OK);
        }
    }
    opts.estimate_only = false;
    let r = p.resummarize(&opts).await?;
    if ctx.json {
        ctx.out_json("sb.resummarize/v1", serde_json::to_value(&r)?);
    } else {
        print_summary_stats(&r);
        eprintln!(
            "{} already at the target generator, {} skipped without raw data.",
            r.stats.already_current, r.stats.no_raw
        );
        for e in r.errors.iter().take(10) {
            eprintln!("  problem: {e}");
        }
        stop_hint(&r.stop, "the same sb resummarize command");
    }
    Ok(stop_exit(&r.stop, r.stats.failed > 0))
}

/// `sb ingest` (docs/specs/ingest.md).
pub async fn ingest(ctx: &Ctx, a: IngestArgs) -> anyhow::Result<i32> {
    use sb_pipeline::ingest::{IngestOptions, IngestStatus};
    let locators: Vec<String> = a
        .locators
        .iter()
        .flat_map(|l| sb_ondemand::local::expand_wildcards(l))
        .collect();
    let opts = IngestOptions {
        account: a.account.clone(),
        title: a.title.clone(),
        context: a.context.clone(),
        date: a
            .date
            .as_deref()
            .map(|d| parse_date(d, false))
            .transpose()?,
        force: a.force,
        keep_original: a.keep_original,
        no_summary: a.no_summary,
        dry_run: a.dry_run,
    };
    let p = ctx.pipeline()?;
    let r = p.ingest(&locators, &opts).await?;
    if r.valid == 0 {
        let why = r
            .results
            .first()
            .and_then(|x| x.message.clone())
            .unwrap_or_else(|| "no valid locator".into());
        return Err(usage(why));
    }
    let summarized = r
        .summarize
        .as_ref()
        .map(|s| s.stats.summarized)
        .unwrap_or(0);
    let stopped = match &r.stop {
        Some(Stop::Cancelled) => Some("interrupted".to_string()),
        Some(Stop::Limit(l)) => Some(l.clone()),
        None => None,
    };
    if ctx.json {
        let status = if r.has_problems() { "partial" } else { "ok" };
        ctx.out_json(
            "sb.ingest/v1",
            json!({
                "run": {"status": status, "summarized": summarized, "pending": r.pending, "stopped": stopped},
                "results": r.results,
            }),
        );
    } else {
        for x in &r.results {
            let label = serde_json::to_value(x.status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let name = x.title.clone().unwrap_or_else(|| x.locator.clone());
            let place = match (&x.source_kind, &x.account) {
                (Some(k), Some(a)) => format!("  [{k} {a}]"),
                (Some(k), None) => format!("  [{k}]"),
                _ => String::new(),
            };
            match x.status {
                IngestStatus::Failed | IngestStatus::NotApplicable => println!(
                    "{label:<14} {}: {}",
                    x.locator,
                    x.message.as_deref().unwrap_or("")
                ),
                IngestStatus::Duplicate => println!(
                    "{label:<14} {name}{place}  {}",
                    x.message.as_deref().unwrap_or("")
                ),
                _ => {
                    println!(
                        "{label:<14} {name}{place}  {}",
                        x.entry_uid.as_deref().unwrap_or("")
                    );
                    if let Some(m) = &x.message {
                        eprintln!("  note: {m}");
                    }
                }
            }
        }
        if r.pending > 0 {
            eprintln!(
                "{} entries still wait for a summary; `sb summarize` or the next `sb sync` will do it.",
                r.pending
            );
        }
        stop_hint(&r.stop, "sb summarize");
    }
    Ok(match &r.stop {
        Some(Stop::Cancelled) => exit::INTERRUPTED,
        _ if r.has_problems() => exit::PROBLEMS,
        _ => exit::OK,
    })
}

pub fn import(ctx: &Ctx, a: ImportArgs) -> anyhow::Result<i32> {
    let maps = a
        .maps
        .iter()
        .map(|m| {
            m.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| usage(format!("invalid --map {m:?} (use kind=account)")))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let p = ctx.pipeline()?;
    let r = p.import_bundle(&a.bundle, &maps, a.dry_run)?;
    if ctx.json {
        ctx.out_json("sb.import/v1", serde_json::to_value(&r)?);
    } else {
        eprintln!(
            "{}{} lines: {} created, {} updated, {} unchanged, {} merged into entries with raw data, {} invalid",
            if r.dry_run { "Dry run: " } else { "" },
            r.lines,
            r.created,
            r.updated,
            r.unchanged,
            r.merged_into_present,
            r.invalid
        );
        for e in r.errors.iter().take(20) {
            eprintln!("  line {}: {}", e.line, e.message);
        }
        if r.raw_missing > 0 {
            eprintln!(
                "{} entries have no raw data (raw_status = missing). Fetch it later with `sb refetch --raw-missing`.",
                r.raw_missing
            );
        }
    }
    Ok(if r.stop.is_some() {
        exit::INTERRUPTED
    } else if r.invalid > 0 {
        exit::PROBLEMS
    } else {
        exit::OK
    })
}
