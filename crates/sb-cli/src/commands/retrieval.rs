//! `sb search`, `show`, `list`, `stats`, `review`.

use std::collections::HashMap;

use sb_core::search::{SearchMode, SearchQuery};
use sb_core::{Language, RawRole, SectionKind, SectionOrigin, SourceKind};
use sb_store::{Catalog, Entry, EntryFilter};
use serde_json::{Value, json};

use crate::Ctx;
use crate::cli::{ListArgs, ReviewArgs, SearchArgs, ShowArgs};
use crate::util::{
    exit, failure, filter_from, parse_date, parse_sections, parse_source_kinds, usage,
};

/// Account labels by ID.
fn labels(cat: &Catalog) -> anyhow::Result<HashMap<String, String>> {
    Ok(cat
        .accounts()?
        .into_iter()
        .map(|a| (a.id.to_string(), a.label))
        .collect())
}

/// `cite_url`: `source_url`, then `metadata.transcript_url`, then the local raw path.
pub(crate) fn cite_url(cat: &Catalog, e: &Entry) -> anyhow::Result<Option<String>> {
    if let Some(u) = e.cite_url() {
        return Ok(Some(u));
    }
    Ok(cat
        .raw_objects(e.id)?
        .first()
        .map(|r| cat.home().resolve_rel(&r.path).display().to_string()))
}

fn account_json(labels: &HashMap<String, String>, id: &str) -> Value {
    json!({"id": id, "label": labels.get(id).cloned().unwrap_or_else(|| id.to_string())})
}

/// Entry date in local time for human output.
fn date_str(e: &Entry) -> String {
    e.date()
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

pub fn search(ctx: &Ctx, a: SearchArgs) -> anyhow::Result<i32> {
    let cat = ctx.catalog()?;
    let accounts = a
        .accounts
        .iter()
        .map(|s| sb_core::AccountId::new(s.clone()).map_err(|e| usage(e.to_string())))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let q = SearchQuery {
        terms: a.terms.clone(),
        sections: parse_sections(&a.sections)?,
        source_kinds: parse_source_kinds(&a.sources)?,
        accounts,
        since: a
            .since
            .as_deref()
            .map(|s| parse_date(s, false))
            .transpose()?,
        until: a
            .until
            .as_deref()
            .map(|s| parse_date(s, true))
            .transpose()?,
        limit: a.limit,
        mode: SearchMode::FullText,
        all_sections: a.all_sections,
    };
    let hits = sb_store::fts::search(cat.conn(), &q).map_err(|e| usage(e.to_string()))?;
    let labels = labels(&cat)?;
    let lang = ctx.language(&cat);
    let mut out = Vec::new();
    for h in &hits {
        let Some(e) = cat.entry(h.entry_id)? else {
            continue;
        };
        out.push((e, h));
    }
    if ctx.json {
        let hits: Vec<Value> = out
            .iter()
            .map(|(e, h)| {
                Ok(json!({
                    "entry_uid": e.entry_uid,
                    "title": e.title,
                    "source_kind": e.source_kind,
                    "account": account_json(&labels, &e.account_id),
                    "date": sb_core::util::ts(e.date()),
                    "section": h.section,
                    "snippet": h.snippet,
                    "score": h.score,
                    "cite_url": cite_url(&cat, e)?,
                }))
            })
            .collect::<anyhow::Result<_>>()?;
        let section = if q.sections.len() == 1 {
            json!(q.sections[0])
        } else {
            json!(q.sections)
        };
        ctx.out_json(
            "sb.search/v1",
            json!({"query": {"terms": q.terms, "section": section, "source": q.source_kinds, "account": a.accounts,
                              "since": q.since.map(sb_core::util::ts), "until": q.until.map(sb_core::util::ts),
                              "limit": q.effective_limit()},
                   "hits": hits}),
        );
        return Ok(exit::OK);
    }
    if out.is_empty() {
        println!("No hits.");
        return Ok(exit::OK);
    }
    for (i, (e, h)) in out.iter().enumerate() {
        println!(
            "{}. [{}] {} ({}, {}) — {}",
            i + 1,
            date_str(e),
            e.title,
            e.source_kind,
            labels
                .get(&e.account_id)
                .map(String::as_str)
                .unwrap_or(&e.account_id),
            h.section.label(lang)
        );
        println!("   {}", h.snippet);
        if let Some(u) = cite_url(&cat, e)? {
            println!("   {u}");
        }
        println!("   id: {}", e.entry_uid);
    }
    Ok(exit::OK)
}

fn raw_text(cat: &Catalog, e: &Entry, role: Option<RawRole>) -> anyhow::Result<String> {
    let rows: Vec<_> = cat
        .raw_objects(e.id)?
        .into_iter()
        .filter(|r| role.is_none_or(|x| x == r.role))
        .collect();
    let segs = sb_store::rawstore::read_segments(cat.home(), &rows)?;
    Ok(segs
        .iter()
        .map(|s| String::from_utf8_lossy(&s.bytes).to_string())
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn show(ctx: &Ctx, a: ShowArgs) -> anyhow::Result<i32> {
    let cat = ctx.catalog()?;
    let Some(e) = cat.entry_by_uid(&a.entry_uid)? else {
        return Err(failure(
            "entry.not_found",
            format!("no entry {}", a.entry_uid),
        ));
    };
    let wanted = parse_sections(&a.sections)?;
    let sections: Vec<_> = cat
        .sections(e.id)?
        .into_iter()
        .filter(|s| wanted.is_empty() || wanted.contains(&s.kind))
        .collect();
    let summary = cat.summary(e.id)?;
    let labels = labels(&cat)?;
    let role = a
        .role
        .as_deref()
        .map(|r| r.parse::<RawRole>().map_err(|e| usage(e.to_string())))
        .transpose()?;
    let raw = if a.raw {
        cat.raw_objects(e.id)?
    } else {
        vec![]
    };
    let raw_content = if a.raw && role.is_some() {
        Some(raw_text(&cat, &e, role)?)
    } else {
        None
    };
    let lang = ctx.language(&cat);
    if ctx.json {
        let mut entry = json!({
            "entry_uid": e.entry_uid,
            "title": e.title,
            "source_kind": e.source_kind,
            "source_id": e.source_id,
            "account": account_json(&labels, &e.account_id),
            "date": sb_core::util::ts(e.date()),
            "source_created_at": e.source_created_at.map(sb_core::util::ts),
            "source_updated_at": e.source_updated_at.map(sb_core::util::ts),
            "ingested_at": sb_core::util::ts(e.ingested_at),
            "raw_status": e.raw_status,
            "summary_status": e.summary_status,
            "cite_url": cite_url(&cat, &e)?,
            "summary": summary.as_ref().map(|s| json!({
                "generator_kind": s.generator_kind, "provider": s.provider, "model": s.model,
                "profile": s.profile, "prompt_version": s.prompt_version,
                "generated_at": sb_core::util::ts(s.generated_at)})),
            "sections": sections.iter().map(|s| json!({"kind": s.kind, "origin": s.origin, "text": s.text})).collect::<Vec<_>>(),
        });
        if a.meta {
            entry["metadata"] = e.metadata.clone();
        }
        if a.raw {
            entry["raw"] = json!(
                raw.iter()
                    .map(|r| json!({
                "role": r.role, "seq": r.seq, "media_type": r.media_type, "size": r.size,
                "path": cat.home().resolve_rel(&r.path).display().to_string()}))
                    .collect::<Vec<_>>()
            );
        }
        if let Some(c) = raw_content {
            entry["raw_content"] = json!(c);
        }
        ctx.out_json("sb.show/v1", json!({"entry": entry}));
        return Ok(exit::OK);
    }
    if let Some(c) = raw_content {
        print!("{c}");
        if !c.ends_with('\n') {
            println!();
        }
        return Ok(exit::OK);
    }
    println!("# {}\n", e.title);
    println!("- Date: {}", date_str(&e));
    println!(
        "- Source: {} ({})",
        e.source_kind,
        labels
            .get(&e.account_id)
            .map(String::as_str)
            .unwrap_or(&e.account_id)
    );
    if let Some(u) = cite_url(&cat, &e)? {
        println!("- Link: {u}");
    }
    if let Some(s) = &summary {
        println!(
            "- Summary by: {}/{}/{}",
            s.generator_kind, s.provider, s.model
        );
    }
    println!("- ID: {}", e.entry_uid);
    if a.meta {
        println!("- Metadata: {}", serde_json::to_string_pretty(&e.metadata)?);
    }
    if a.raw {
        println!("\nRaw files:");
        for r in &raw {
            println!(
                "  {} #{} {} ({} bytes)",
                r.role,
                r.seq,
                cat.home().resolve_rel(&r.path).display(),
                r.size
            );
        }
        if raw.is_empty() {
            println!("  (none: raw_status = {})", e.raw_status);
        }
        return Ok(exit::OK);
    }
    for s in &sections {
        println!("\n## {}\n\n{}", s.kind.label(lang), s.text);
    }
    Ok(exit::OK)
}

fn entry_row(cat: &Catalog, labels: &HashMap<String, String>, e: &Entry) -> anyhow::Result<Value> {
    Ok(json!({
        "entry_uid": e.entry_uid,
        "title": e.title,
        "source_kind": e.source_kind,
        "account": account_json(labels, &e.account_id),
        "date": sb_core::util::ts(e.date()),
        "raw_status": e.raw_status,
        "summary_status": e.summary_status,
        "cite_url": cite_url(cat, e)?,
    }))
}

pub fn list(ctx: &Ctx, a: ListArgs) -> anyhow::Result<i32> {
    let cat = ctx.catalog()?;
    let mut f = filter_from(&a.filters)?;
    f.limit = Some(a.limit.clamp(1, 1000));
    let entries = cat.list_entries(&f)?;
    let labels = labels(&cat)?;
    if ctx.json {
        let rows: Vec<Value> = entries
            .iter()
            .map(|e| entry_row(&cat, &labels, e))
            .collect::<anyhow::Result<_>>()?;
        ctx.out_json("sb.list/v1", json!({"entries": rows, "total": cat.count_entries(&EntryFilter { limit: None, ..f })?}));
        return Ok(exit::OK);
    }
    for e in &entries {
        println!(
            "{}  {:<12} {:<8} {:<8} {}  {}",
            date_str(e),
            e.source_kind,
            e.raw_status,
            e.summary_status,
            e.entry_uid,
            e.title
        );
    }
    if entries.is_empty() {
        println!("No entries.");
    }
    Ok(exit::OK)
}

pub fn stats(ctx: &Ctx) -> anyhow::Result<i32> {
    let cat = ctx.catalog()?;
    let s = cat.stats()?;
    let policy = sb_pipeline::policy::SummaryPolicy::load(&cat)?;
    let b = sb_pipeline::budget::status(&cat, &policy)?;
    if ctx.json {
        let mut v = serde_json::to_value(&s)?;
        if let Some(o) = v.as_object_mut() {
            let one = |p: &sb_pipeline::budget::PeriodStatus| json!({"spent_usd": p.spent_usd, "cap_usd": p.cap_usd, "resets_at": p.resets_at});
            o.insert(
                "budget".into(),
                json!({"weekly": one(&b.week), "monthly": one(&b.month)}),
            );
        }
        ctx.out_json("sb.stats/v1", v);
        return Ok(exit::OK);
    }
    println!(
        "Entries: {} ({} sections), queued fetches: {}",
        s.entries, s.sections, s.queue
    );
    println!("\nBy account and source:");
    for c in &s.by_source {
        println!(
            "  {:<16} {:<13} {:>7}  {} .. {}",
            c.account_id,
            c.source_kind,
            c.count,
            c.oldest.as_deref().unwrap_or("-").get(..10).unwrap_or("-"),
            c.newest.as_deref().unwrap_or("-").get(..10).unwrap_or("-")
        );
    }
    println!("\nRaw status:");
    for c in &s.by_raw_status {
        println!("  {:<14} {:>7}", c.key, c.count);
    }
    println!("\nSummary status:");
    for c in &s.by_summary_status {
        println!("  {:<14} {:>7}", c.key, c.count);
    }
    println!("\nSummaries by generator:");
    for c in &s.by_summary_model {
        println!("  {:<48} {:>7}", c.key, c.count);
    }
    println!("\nSummarization budget (estimated; see `sb budget`):");
    super::budget::print_current(&b.week, &b.month, true);
    Ok(exit::OK)
}

/// The source text to review next to the summary.
fn source_text(cat: &Catalog, e: &Entry) -> anyhow::Result<String> {
    if e.source_kind == SourceKind::GoogleMeet && e.raw_status == sb_core::RawStatus::Present {
        let t = raw_text(cat, e, Some(RawRole::Transcript))?;
        if !t.trim().is_empty() {
            return Ok(sb_google::gemini::clean(&t));
        }
        let notes = raw_text(cat, e, Some(RawRole::Notes))?;
        if let Some(p) = sb_google::gemini::parse(&notes)
            && let Some(t) = p.transcript
        {
            return Ok(t);
        }
        return Ok(sb_google::gemini::clean(&notes));
    }
    Ok(cat
        .sections(e.id)?
        .into_iter()
        .find(|s| s.origin == SectionOrigin::Extracted && s.kind == SectionKind::Details)
        .map(|s| s.text)
        .unwrap_or_default())
}

pub fn review(ctx: &Ctx, a: ReviewArgs) -> anyhow::Result<i32> {
    let cat = ctx.catalog()?;
    let mut f = filter_from(&a.filters)?;
    f.channel = a.channel.clone();
    f.limit = Some(a.limit.clamp(1, 200));
    if !a.all {
        f.has_summary = Some(true);
    }
    let lang: Language = ctx.language(&cat);
    let mut items = Vec::new();
    for e in cat.list_entries(&f)? {
        let generated: Vec<_> = cat
            .sections(e.id)?
            .into_iter()
            .filter(|s| s.origin == SectionOrigin::Generated)
            .collect();
        let text = source_text(&cat, &e)?;
        let total = text.lines().count();
        let shown: String = if a.full {
            text.clone()
        } else {
            text.lines()
                .take(a.detail_lines)
                .collect::<Vec<_>>()
                .join("\n")
        };
        items.push((e, generated, shown, total));
    }
    if ctx.json {
        let rows: Vec<Value> = items
            .iter()
            .map(|(e, g, shown, total)| {
                Ok(json!({
                    "entry_uid": e.entry_uid, "title": e.title, "source_kind": e.source_kind,
                    "date": sb_core::util::ts(e.date()), "cite_url": cite_url(&cat, e)?,
                    "summary": cat.summary(e.id)?.map(|s| json!({"provider": s.provider, "model": s.model})),
                    "generated": g.iter().map(|s| json!({"kind": s.kind, "text": s.text})).collect::<Vec<_>>(),
                    "source_text": shown,
                    "source_lines": total,
                }))
            })
            .collect::<anyhow::Result<_>>()?;
        ctx.out_json("sb.review/v1", json!({"entries": rows}));
        return Ok(exit::OK);
    }
    for (e, g, shown, total) in &items {
        println!("================================================================");
        println!("{}  {}", date_str(e), e.title);
        if let Some(s) = cat.summary(e.id)? {
            println!("summary by {}/{}", s.provider, s.model);
        }
        if let Some(u) = cite_url(&cat, e)? {
            println!("{u}");
        }
        for s in g {
            println!("\n## {}\n{}", s.kind.label(lang), s.text);
        }
        if *total == 0 {
            println!("\n---- source: none (raw_status = {}) ----", e.raw_status);
        } else {
            println!("\n---- source ({total} lines) ----\n{shown}");
        }
        if !a.full && *total > a.detail_lines {
            println!("... ({} more lines; use --full)", total - a.detail_lines);
        }
    }
    if items.is_empty() {
        println!("No entries to review.");
    }
    Ok(exit::OK)
}
