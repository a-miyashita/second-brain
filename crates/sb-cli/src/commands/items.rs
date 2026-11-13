//! `sb recent` and `sb items` (ADR-0024, items.md).

use second_brain_kernel::{
    Confidence, EffectiveStatus, ItemKind, SectionKind, StoredItem, util::ts,
};
use second_brain_pipeline::PipelineError;
use second_brain_pipeline::items::{
    ItemSettings, ItemView, ItemsQuery, RecentEntry, Who, load_aliases, query_items,
    recent as recent_entries,
};
use second_brain_store::{Catalog, Entry, RecentFilter};
use serde_json::{Value, json};

use crate::Ctx;
use crate::cli::{ItemsArgs, RecentArgs};
use crate::commands::retrieval::{account_json, cite_url, labels};
use crate::util::{CliError, exit, parse_sections, parse_source_kinds, parse_when, usage};

use std::collections::HashMap;

/// The sections `sb recent` shows by default.
const DEFAULT_SECTIONS: [SectionKind; 3] = [
    SectionKind::Overview,
    SectionKind::Decisions,
    SectionKind::ActionItems,
];

fn local(t: chrono::DateTime<chrono::Utc>) -> String {
    t.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

fn bullets(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.trim().trim_start_matches("- ").to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// The fields of an item that do not depend on how it was found.
fn stored_item_json(entry_uid: &str, it: &StoredItem) -> Value {
    let i = &it.item;
    json!({
        "item_ref": format!("{entry_uid}#{}", i.ordinal),
        "kind": i.kind,
        "text": i.text,
        "status": i.status,
        "owner": i.owner,
        "requester": i.requester,
        "due": i.due,
        "actors": i.actors,
        "confidence": i.confidence,
        "evidence": i.evidence,
        "occurred_at": i.occurred_at.map(ts),
        "scope_key": i.scope_key,
    })
}

fn entry_brief(
    cat: &Catalog,
    labels: &HashMap<String, String>,
    e: &Entry,
) -> anyhow::Result<Value> {
    Ok(json!({
        "entry_uid": e.entry_uid,
        "title": e.title,
        "source_kind": e.source_kind,
        "account": account_json(labels, &e.account_id),
        "date": ts(e.date()),
        "cite_url": cite_url(cat, e)?,
    }))
}

pub fn recent_cmd_json(
    cat: &Catalog,
    labels: &HashMap<String, String>,
    r: &RecentEntry,
) -> anyhow::Result<Value> {
    let e = &r.entry;
    let mut sections = serde_json::Map::new();
    for s in &r.sections {
        let v = match s.kind {
            SectionKind::Decisions | SectionKind::ActionItems => json!(bullets(&s.text)),
            _ => json!(s.text),
        };
        sections.insert(s.kind.to_string(), v);
    }
    let mut v = json!({
        "entry_uid": e.entry_uid,
        "title": e.title,
        "source_kind": e.source_kind,
        "account": account_json(labels, &e.account_id),
        "activity_at": ts(r.activity_at),
        "cite_url": cite_url(cat, e)?,
        "summarized": r.summarized,
        "items_available": r.items_available,
        "sections": sections,
        "items": r.items.iter().map(|i| stored_item_json(&e.entry_uid, i)).collect::<Vec<_>>(),
    });
    if let Some(x) = &r.details_excerpt
        && let Some(o) = v.as_object_mut()
    {
        o.insert("details_excerpt".into(), json!(x));
    }
    Ok(v)
}

pub fn recent(ctx: &Ctx, a: RecentArgs) -> anyhow::Result<i32> {
    let cat = ctx.catalog()?;
    let now = cat.now();
    let since = parse_when(&a.since, now)?;
    let until = a.until.as_deref().map(|s| parse_when(s, now)).transpose()?;
    let mut sections = parse_sections(&a.sections)?;
    if sections.is_empty() {
        sections = DEFAULT_SECTIONS.to_vec();
    }
    let filter = RecentFilter {
        accounts: a.accounts.clone(),
        source_kinds: parse_source_kinds(&a.sources)?,
        since: Some(since),
        until,
        limit: Some(a.limit.clamp(1, 200)),
    };
    let (entries, truncated) = recent_entries(&cat, &filter, &sections, a.with_items)?;
    let labels = labels(&cat)?;
    if ctx.json {
        let rows = entries
            .iter()
            .map(|r| recent_cmd_json(&cat, &labels, r))
            .collect::<anyhow::Result<Vec<_>>>()?;
        ctx.out_json(
            "sb.recent/v1",
            json!({
                "query": {"since": ts(since), "until": until.map(ts), "limit": filter.limit},
                "entries": rows,
                "total": entries.len(),
                "truncated": truncated,
            }),
        );
        return Ok(exit::OK);
    }
    let lang = ctx.language(&cat);
    if entries.is_empty() {
        println!("No entries since {}.", local(since));
    }
    for r in &entries {
        println!(
            "{}  {:<12} {}  ({})",
            local(r.activity_at),
            r.entry.source_kind,
            r.entry.title,
            r.entry.entry_uid
        );
        if !r.summarized {
            println!("  (no summary yet)");
            if let Some(x) = &r.details_excerpt {
                println!("  {}", x.replace('\n', "\n  "));
            }
        }
        for s in &r.sections {
            println!(
                "  {}:\n    {}",
                s.kind.label(lang),
                s.text.replace('\n', "\n    ")
            );
        }
        for i in &r.items {
            println!("  - [{}] {}", i.item.kind.as_str(), i.item.text);
        }
    }
    if truncated {
        println!("\nMore entries exist; narrow the range or raise --limit.");
    }
    Ok(exit::OK)
}

fn parse_conf(s: &str) -> anyhow::Result<Confidence> {
    s.parse::<Confidence>().map_err(usage)
}

/// A `YYYY-MM-DD` for `--due-by`: `today`, a date, or any time/age (local date).
fn due_date(s: &str, now: chrono::DateTime<chrono::Utc>) -> anyhow::Result<String> {
    let t = s.trim();
    if chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d").is_ok() {
        return Ok(t.to_string());
    }
    let at = parse_when(t, now)?;
    Ok(at
        .with_timezone(&chrono::Local)
        .date_naive()
        .format("%Y-%m-%d")
        .to_string())
}

fn item_json(
    cat: &Catalog,
    labels: &HashMap<String, String>,
    v: &ItemView,
) -> anyhow::Result<Value> {
    let r = &v.row;
    let mut j = stored_item_json(&r.entry.entry_uid, &r.stored);
    let candidates = v
        .candidates
        .iter()
        .map(|c| {
            let mut reason = Vec::new();
            if c.same_scope {
                reason.push("same_scope");
            }
            if c.shared_person {
                reason.push("shared_person");
            }
            let mut cj = stored_item_json(&c.row.entry.entry_uid, &c.row.stored);
            if let Some(o) = cj.as_object_mut() {
                o.insert(
                    "similarity".into(),
                    json!((c.similarity * 1000.0).round() / 1000.0),
                );
                o.insert("reason".into(), json!(reason));
                o.insert(
                    "entry".into(),
                    entry_brief(cat, labels, &c.row.entry).unwrap_or(Value::Null),
                );
            }
            cj
        })
        .collect::<Vec<_>>();
    if let Some(o) = j.as_object_mut() {
        o.insert("effective_status".into(), json!(v.effective_status));
        o.insert("stale".into(), json!(v.stale));
        o.insert("mine".into(), json!(v.mine));
        o.insert("entry".into(), entry_brief(cat, labels, &r.entry)?);
        o.insert(
            "extracted_by".into(),
            json!({"model": r.model, "profile": r.profile, "prompt_version": r.prompt_version}),
        );
        o.insert("resolved_by".into(), json!([]));
        o.insert("candidates".into(), json!(candidates));
    }
    Ok(j)
}

pub fn items(ctx: &Ctx, a: ItemsArgs) -> anyhow::Result<i32> {
    let cat = ctx.catalog()?;
    let now = cat.now();
    let kinds = a
        .kinds
        .iter()
        .map(|k| k.parse::<ItemKind>().map_err(usage))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let statuses = a
        .statuses
        .iter()
        .map(|s| s.parse::<EffectiveStatus>().map_err(usage))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let who = if a.mine {
        Who::Mine
    } else if a.asked_by_me {
        Who::AskedByMe
    } else if let Some(n) = &a.owner {
        Who::Owner(n.clone())
    } else {
        Who::Anyone
    };
    let query = ItemsQuery {
        kinds,
        statuses,
        who: who.clone(),
        since: a.since.as_deref().map(|s| parse_when(s, now)).transpose()?,
        until: a.until.as_deref().map(|s| parse_when(s, now)).transpose()?,
        due_by: a.due_by.as_deref().map(|s| due_date(s, now)).transpose()?,
        accounts: a.accounts.clone(),
        source_kinds: parse_source_kinds(&a.sources)?,
        scope: a.scope.clone(),
        min_confidence: a.min_confidence.as_deref().map(parse_conf).transpose()?,
        stale: if a.stale {
            Some(true)
        } else if a.not_stale {
            Some(false)
        } else {
            None
        },
        candidates: a.candidates,
        limit: a.limit.clamp(1, 200) as usize,
    };
    let settings = ItemSettings::load(&cat)?;
    let aliases = load_aliases(&cat)?;
    let answer = match query_items(&cat, &query, &aliases, &settings, now) {
        Ok(x) => x,
        Err(PipelineError::AliasesEmpty) => {
            return Err(CliError {
                code: "aliases.empty",
                exit: exit::USAGE,
                message: PipelineError::AliasesEmpty.to_string(),
            }
            .into());
        }
        Err(e) => return Err(e.into()),
    };
    let labels = labels(&cat)?;
    if ctx.json {
        let rows = answer
            .items
            .iter()
            .map(|v| item_json(&cat, &labels, v))
            .collect::<anyhow::Result<Vec<_>>>()?;
        ctx.out_json(
            "sb.items/v1",
            json!({
                "query": {
                    "kinds": query.kinds,
                    "status": query.statuses,
                    "mine": who == Who::Mine,
                    "asked_by_me": who == Who::AskedByMe,
                    "min_confidence": query.min_confidence.unwrap_or(settings.min_confidence),
                },
                "items": rows,
                "total": answer.total,
                "hidden_low_confidence": answer.hidden_low_confidence,
                "coverage": answer.coverage,
            }),
        );
        return Ok(exit::OK);
    }
    for v in &answer.items {
        let i = &v.row.stored.item;
        let status = v
            .effective_status
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|| i.kind.as_str().to_string());
        let mut extra = Vec::new();
        if let Some(o) = &i.owner {
            extra.push(o.clone());
        }
        if let Some(d) = &i.due {
            extra.push(format!("due {d}"));
        }
        if v.stale {
            extra.push("stale".into());
        }
        let extra = if extra.is_empty() {
            String::new()
        } else {
            format!("  ({})", extra.join(", "))
        };
        println!("[{status}] {}{extra}", i.text);
        println!(
            "    {} · {} · {}",
            v.row.entry.title,
            i.occurred_at.map(local).unwrap_or_default(),
            cite_url(&cat, &v.row.entry)?.unwrap_or_default()
        );
        if let Some(q) = &i.evidence {
            println!("    \"{q}\"");
        }
        for c in &v.candidates {
            println!(
                "    ? maybe closed by: {} ({})",
                c.row.stored.item.text, c.row.entry.title
            );
        }
    }
    if answer.items.is_empty() {
        println!("No items.");
    }
    let c = &answer.coverage;
    println!(
        "\n{} of {} entries in range have no items ({} unsummarized, {} from older prompts, {} native). {} hidden by the confidence floor.",
        c.entries_without_items,
        c.entries_in_range,
        c.unsummarized,
        c.predate_items,
        c.native,
        answer.hidden_low_confidence
    );
    Ok(exit::OK)
}
