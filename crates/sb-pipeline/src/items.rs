//! Items at query time: aliases, effective status, staleness, candidate pairs and
//! `sb recent` (ADR-0024, items.md). Everything here reads the catalog; nothing
//! calls an LLM.

use chrono::{DateTime, Duration, Utc};
use second_brain_kernel::{
    AliasSet, Confidence, EffectiveStatus, ItemCounts, ItemCtx, ItemKind, MineMatch, NewItem,
    SectionKind, SectionOrigin, SourceKind, StoredItem, SummaryInput, SummaryOutput, SummaryStatus,
    candidates_for, effective_status, is_stale, prompt_extracts_items, validate_items,
};
use second_brain_store::{
    Account, Catalog, Entry, EntryFilter, ItemFilter, ItemRow, ItemsCoverage, RecentFilter, Section,
};
use serde_json::Value;

use crate::error::PipelineError;

/// The `items.*` settings.
#[derive(Debug, Clone)]
pub struct ItemSettings {
    pub min_confidence: Confidence,
    pub max_per_entry: usize,
    pub stale_days: i64,
    pub candidate_days: i64,
    pub candidate_min_similarity: f64,
    pub link_min_confidence: Confidence,
}

impl ItemSettings {
    pub fn load(cat: &Catalog) -> Result<Self, PipelineError> {
        let conf = |key: &str, fallback: Confidence| -> Result<Confidence, PipelineError> {
            Ok(cat
                .setting_or::<String>(key, fallback.as_str().to_string())?
                .parse()
                .unwrap_or(fallback))
        };
        Ok(ItemSettings {
            min_confidence: conf("items.min_confidence", Confidence::Medium)?,
            max_per_entry: cat.setting_or("items.max_per_entry", 30usize)?.max(1),
            stale_days: cat.setting_or("items.stale_days", 30i64)?.max(0),
            candidate_days: cat.setting_or("items.candidate_days", 60i64)?.max(1),
            candidate_min_similarity: cat
                .setting_or("items.candidate_min_similarity", 0.2f64)?
                .clamp(0.0, 1.0),
            link_min_confidence: conf("items.link_min_confidence", Confidence::Medium)?,
        })
    }
}

/// The user's names and emails across all accounts.
#[derive(Debug, Clone, Default)]
pub struct Aliases {
    pub set: AliasSet,
    /// Names in the order found, without duplicates (the prompt hint).
    pub names: Vec<String>,
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Collect the aliases of every enabled account: `aliases` (collected by `sb`)
/// and `extra_aliases` (written by the user), see accounts-and-auth.md.
pub fn load_aliases(cat: &Catalog) -> Result<Aliases, PipelineError> {
    let mut names: Vec<String> = Vec::new();
    let mut emails: Vec<String> = Vec::new();
    for a in cat.accounts()? {
        if a.status == second_brain_kernel::AccountStatus::Disabled {
            continue;
        }
        for key in ["aliases", "extra_aliases"] {
            let Some(o) = a.config.get(key) else { continue };
            for n in strings(o.get("names")) {
                if !names.iter().any(|x| x.eq_ignore_ascii_case(&n)) {
                    names.push(n);
                }
            }
            for e in strings(o.get("emails")) {
                if !emails.iter().any(|x| x.eq_ignore_ascii_case(&e)) {
                    emails.push(e);
                }
            }
        }
    }
    Ok(Aliases {
        set: AliasSet::new(&names, &emails),
        names,
    })
}

/// Validate the items of a summary output against the entry (items.md "Validation").
pub fn validate_output(
    cat: &Catalog,
    out: &SummaryOutput,
    input: &SummaryInput,
    e: &Entry,
) -> Result<(Vec<NewItem>, ItemCounts), PipelineError> {
    let tz = cat.setting_or::<String>("slack.day_timezone", "UTC".into())?;
    let max = cat.setting_or("items.max_per_entry", 30usize)?.max(1);
    let ctx = ItemCtx {
        source_kind: e.source_kind,
        source_id: &e.source_id,
        title: &e.title,
        metadata: &e.metadata,
        source_created_at: e.source_created_at,
        source_updated_at: e.source_updated_at,
        timezone: &tz,
        max_items: max,
    };
    Ok(validate_items(&out.items, &input.body, &ctx))
}

/// Who an item query is about.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Who {
    #[default]
    Anyone,
    /// The owner (or an actor) is the user.
    Mine,
    /// The owner matches this name.
    Owner(String),
    /// The requester is the user.
    AskedByMe,
}

/// A query for `sb items`.
#[derive(Debug, Clone, Default)]
pub struct ItemsQuery {
    pub kinds: Vec<ItemKind>,
    /// Matched against the effective status. Empty = any.
    pub statuses: Vec<EffectiveStatus>,
    pub who: Who,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub due_by: Option<String>,
    pub accounts: Vec<String>,
    pub source_kinds: Vec<SourceKind>,
    pub scope: Option<String>,
    pub min_confidence: Option<Confidence>,
    /// `Some(true)` = only stale, `Some(false)` = not stale.
    pub stale: Option<bool>,
    pub candidates: bool,
    pub limit: usize,
}

/// A possible resolution of an open item.
#[derive(Debug, Clone)]
pub struct CandidateView {
    pub row: ItemRow,
    pub similarity: f64,
    pub same_scope: bool,
    pub shared_person: bool,
}

/// One item of an answer.
#[derive(Debug, Clone)]
pub struct ItemView {
    pub row: ItemRow,
    pub effective_status: Option<EffectiveStatus>,
    pub stale: bool,
    pub mine: Option<MineMatch>,
    pub candidates: Vec<CandidateView>,
}

/// The answer to an item query.
#[derive(Debug, Clone)]
pub struct ItemsAnswer {
    pub items: Vec<ItemView>,
    /// Matches before the limit.
    pub total: usize,
    /// Matches hidden by the confidence floor.
    pub hidden_low_confidence: usize,
    pub coverage: ItemsCoverage,
}

/// Rows read from SQL before the Rust-side filters (a personal catalog is far below this).
const SQL_ROW_CAP: u64 = 20_000;

/// Run an item query. `aliases` is needed for `Who::Mine` and `Who::AskedByMe`.
pub fn query_items(
    cat: &Catalog,
    q: &ItemsQuery,
    aliases: &Aliases,
    settings: &ItemSettings,
    now: DateTime<Utc>,
) -> Result<ItemsAnswer, PipelineError> {
    if matches!(q.who, Who::Mine | Who::AskedByMe) && aliases.set.is_empty() {
        return Err(PipelineError::AliasesEmpty);
    }
    let floor = q.min_confidence.unwrap_or(settings.min_confidence);
    let rows = cat.query_items(&ItemFilter {
        kinds: q.kinds.clone(),
        accounts: q.accounts.clone(),
        source_kinds: q.source_kinds.clone(),
        since: q.since,
        until: q.until,
        scope: q.scope.clone(),
        due_by: q.due_by.clone(),
        min_confidence: None,
        limit: Some(SQL_ROW_CAP),
    })?;
    let ids: Vec<i64> = rows.iter().map(|r| r.stored.id).collect();
    let links = cat.incoming_links(&ids)?;
    let mut scopes: Vec<String> = rows
        .iter()
        .map(|r| r.stored.item.scope_key.clone())
        .collect();
    scopes.sort();
    scopes.dedup();
    let latest = cat.scope_latest(&scopes)?;
    let owner_alias = match &q.who {
        Who::Owner(n) => Some(AliasSet::new(&[n.as_str()], &[])),
        _ => None,
    };

    let mut hidden = 0usize;
    let mut views: Vec<ItemView> = Vec::new();
    for row in rows {
        let it = &row.stored.item;
        let eff = effective_status(
            it,
            links.get(&row.stored.id).map(Vec::as_slice).unwrap_or(&[]),
            settings.link_min_confidence,
        );
        let stale = is_stale(
            eff,
            it.occurred_at,
            latest.get(&it.scope_key).copied(),
            settings.stale_days,
            now,
        );
        // Kinds without a status match `done` only when they report finished work
        // (resolved, accomplishment); decisions never match a status filter.
        let status_ok = q.statuses.is_empty()
            || match eff {
                Some(s) => q.statuses.contains(&s),
                None => {
                    matches!(it.kind, ItemKind::Resolved | ItemKind::Accomplishment)
                        && q.statuses.contains(&EffectiveStatus::Done)
                }
            };
        if !status_ok {
            continue;
        }
        if let Some(want) = q.stale
            && stale != want
        {
            continue;
        }
        let mine = match &q.who {
            Who::Anyone => None,
            Who::Mine => match aliases.set.item_is_mine(it) {
                Some(m) => Some(m),
                None => continue,
            },
            Who::AskedByMe => match aliases.set.item_asked_by_me(it) {
                Some(m) => Some(m),
                None => continue,
            },
            Who::Owner(_) => {
                let matched = owner_alias
                    .as_ref()
                    .zip(it.owner.as_deref())
                    .and_then(|(a, o)| a.matches(o));
                if matched.is_none() {
                    continue;
                }
                None
            }
        };
        if it.confidence < floor {
            hidden += 1;
            continue;
        }
        views.push(ItemView {
            row,
            effective_status: eff,
            stale,
            mine,
            candidates: Vec::new(),
        });
    }
    if q.kinds == [ItemKind::Action] {
        views.sort_by(|a, b| {
            let (x, y) = (&a.row.stored.item, &b.row.stored.item);
            (x.due.is_none(), &x.due, std::cmp::Reverse(x.occurred_at)).cmp(&(
                y.due.is_none(),
                &y.due,
                std::cmp::Reverse(y.occurred_at),
            ))
        });
    }
    let total = views.len();
    views.truncate(q.limit.max(1));

    if q.candidates {
        attach_candidates(cat, &mut views, settings, now)?;
    }
    let coverage = cat.items_coverage(&RecentFilter {
        accounts: q.accounts.clone(),
        source_kinds: q.source_kinds.clone(),
        since: q.since,
        until: q.until,
        limit: None,
    })?;
    Ok(ItemsAnswer {
        items: views,
        total,
        hidden_low_confidence: hidden,
        coverage,
    })
}

fn attach_candidates(
    cat: &Catalog,
    views: &mut [ItemView],
    settings: &ItemSettings,
    now: DateTime<Utc>,
) -> Result<(), PipelineError> {
    let targets: Vec<usize> = views
        .iter()
        .enumerate()
        .filter(|(_, v)| {
            v.effective_status == Some(EffectiveStatus::Open)
                && matches!(
                    v.row.stored.item.kind,
                    ItemKind::Action | ItemKind::Question
                )
                && v.row.stored.item.occurred_at.is_some()
        })
        .map(|(i, _)| i)
        .collect();
    let Some(from) = targets
        .iter()
        .filter_map(|&i| views[i].row.stored.item.occurred_at)
        .min()
    else {
        return Ok(());
    };
    let to = now.max(
        targets
            .iter()
            .filter_map(|&i| views[i].row.stored.item.occurred_at)
            .max()
            .unwrap_or(from)
            + Duration::days(settings.candidate_days),
    );
    let pool_rows = cat.closure_pool(from, to)?;
    let pool: Vec<(i64, &NewItem, Option<EffectiveStatus>)> = pool_rows
        .iter()
        .map(|r| {
            let eff = r.stored.item.status.map(|s| match s {
                second_brain_kernel::ItemStatus::Done => EffectiveStatus::Done,
                second_brain_kernel::ItemStatus::Dropped => EffectiveStatus::Dropped,
                second_brain_kernel::ItemStatus::Answered => EffectiveStatus::Answered,
                second_brain_kernel::ItemStatus::Open => EffectiveStatus::Open,
            });
            (r.stored.entry_id, &r.stored.item, eff)
        })
        .collect();
    for i in targets {
        let t = &views[i].row.stored;
        let found = candidates_for(
            t.entry_id,
            &t.item,
            &pool,
            settings.candidate_days,
            settings.candidate_min_similarity,
        );
        views[i].candidates = found
            .into_iter()
            .map(|c| CandidateView {
                row: pool_rows[c.index].clone(),
                similarity: c.similarity,
                same_scope: c.same_scope,
                shared_person: c.shared_person,
            })
            .collect();
    }
    Ok(())
}

/// One entry of `sb recent`.
#[derive(Debug, Clone)]
pub struct RecentEntry {
    pub entry: Entry,
    pub activity_at: DateTime<Utc>,
    pub summarized: bool,
    pub items_available: bool,
    pub sections: Vec<Section>,
    /// The first characters of `details` for an entry without a summary.
    pub details_excerpt: Option<String>,
    pub items: Vec<StoredItem>,
}

/// Characters of `details` shown for an unsummarized entry.
pub const DETAILS_EXCERPT_CHARS: usize = 400;

/// Entries by activity time with their summary sections (`sb recent`).
pub fn recent(
    cat: &Catalog,
    f: &RecentFilter,
    sections: &[SectionKind],
    with_items: bool,
) -> Result<(Vec<RecentEntry>, bool), PipelineError> {
    let limit = f.limit.unwrap_or(30);
    let mut probe = f.clone();
    probe.limit = Some(limit + 1);
    let mut entries = cat.recent_entries(&probe)?;
    let truncated = entries.len() as u64 > limit;
    entries.truncate(limit as usize);
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let summary = cat.summary(e.id)?;
        let summarized = e.summary_status == SummaryStatus::Done && summary.is_some();
        let items_available = summarized
            && summary
                .as_ref()
                .and_then(|s| s.prompt_version.as_deref())
                .is_some_and(prompt_extracts_items);
        let all = cat.sections(e.id)?;
        let picked: Vec<Section> = all
            .iter()
            .filter(|s| sections.contains(&s.kind) && s.origin == SectionOrigin::Generated)
            .cloned()
            .collect();
        let details_excerpt = if summarized {
            None
        } else {
            all.iter()
                .find(|s| s.kind == SectionKind::Details)
                .map(|s| s.text.chars().take(DETAILS_EXCERPT_CHARS).collect())
        };
        let items = if with_items {
            cat.items_for_entry(e.id)?
        } else {
            Vec::new()
        };
        let activity_at = e
            .source_updated_at
            .or(e.source_created_at)
            .unwrap_or(e.ingested_at);
        out.push(RecentEntry {
            entry: e,
            activity_at,
            summarized,
            items_available,
            sections: picked,
            details_excerpt,
            items,
        });
    }
    Ok((out, truncated))
}

/// Entries per prompt kind that have a summary but no items (doctor `items.coverage`).
pub fn global_coverage(cat: &Catalog) -> Result<ItemsCoverage, PipelineError> {
    Ok(cat.items_coverage(&RecentFilter::default())?)
}

// ---------------------------------------------------------------------------
// Alias collection (accounts-and-auth.md "Self aliases")

/// The `aliases` object stored in an account `config`.
pub fn aliases_value(names: &[String], emails: &[String], now: DateTime<Utc>) -> Value {
    serde_json::json!({
        "names": names,
        "emails": emails,
        "refreshed_at": second_brain_kernel::util::ts(now),
    })
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    let s = s.trim();
    if !s.is_empty() && !v.iter().any(|x| x.eq_ignore_ascii_case(s)) {
        v.push(s.to_string());
    }
}

/// Names and emails of the Slack user, from the cached user directory.
fn slack_aliases(cat: &Catalog, a: &Account) -> Result<(Vec<String>, Vec<String>), PipelineError> {
    let mut names = Vec::new();
    let mut emails = Vec::new();
    let me = a
        .identity
        .as_deref()
        .and_then(|i| i.split_once(':'))
        .map(|(_, u)| u.to_string());
    let (Some(me), Some(dir)) = (me, cat.cache_get_stale(a.id.as_str(), "slack.users")?) else {
        return Ok((names, emails));
    };
    if let Some(u) = dir.get(&me) {
        for k in ["real_name", "display_name", "name"] {
            if let Some(n) = u.get(k).and_then(Value::as_str) {
                push_unique(&mut names, n);
            }
        }
        if let Some(e) = u.get("email").and_then(Value::as_str) {
            push_unique(&mut emails, e);
        }
    }
    Ok((names, emails))
}

/// The identity email, and names from Meet participant lines and the calendar
/// owner flag that were already stored. No network.
fn google_aliases(cat: &Catalog, a: &Account) -> Result<(Vec<String>, Vec<String>), PipelineError> {
    let mut emails = Vec::new();
    let Some(ident) = a.identity.as_deref().filter(|i| i.contains('@')) else {
        return Ok((Vec::new(), emails));
    };
    push_unique(&mut emails, ident);
    let entries = cat.list_entries(&EntryFilter {
        accounts: vec![a.id.to_string()],
        source_kinds: vec![SourceKind::GoogleMeet],
        ..Default::default()
    })?;
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut bump = |name: &str| {
        let name = name.trim();
        if name.is_empty() || name.contains('@') {
            return;
        }
        match counts
            .iter_mut()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
        {
            Some((_, c)) => *c += 1,
            None => counts.push((name.to_string(), 1)),
        }
    };
    for e in &entries {
        for p in e
            .metadata
            .get("participants")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let same = p
                .get("email")
                .and_then(Value::as_str)
                .is_some_and(|m| m.eq_ignore_ascii_case(ident));
            if same && let Some(n) = p.get("name").and_then(Value::as_str) {
                bump(n);
            }
        }
        for n in strings(e.metadata.get("self_names")) {
            bump(&n);
        }
    }
    counts.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    let names = counts.into_iter().take(3).map(|(n, _)| n).collect();
    Ok((names, emails))
}

/// Recompute the `aliases` of Slack and Google accounts from stored data and
/// write them when they changed. Returns the number of accounts updated.
/// Microsoft aliases are written at sign-in, from the ID token.
pub fn refresh_aliases(cat: &Catalog) -> Result<usize, PipelineError> {
    let now = cat.now();
    let mut changed = 0;
    for a in cat.accounts()? {
        let (names, emails) = match a.kind {
            second_brain_kernel::AccountKind::Slack => slack_aliases(cat, &a)?,
            second_brain_kernel::AccountKind::Google => google_aliases(cat, &a)?,
            _ => continue,
        };
        if names.is_empty() && emails.is_empty() {
            continue;
        }
        let same = a.config.get("aliases").is_some_and(|o| {
            strings(o.get("names")) == names && strings(o.get("emails")) == emails
        });
        if same {
            continue;
        }
        let mut cfg = a.config.clone();
        cfg["aliases"] = aliases_value(&names, &emails, now);
        cat.update_account(&a.id, None, None, Some(&cfg))?;
        changed += 1;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use second_brain_kernel::AccountKind;
    use second_brain_store::Home;
    use serde_json::json;

    fn open() -> (tempfile::TempDir, Catalog) {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::create(&Home::new(dir.path().to_path_buf())).unwrap();
        (dir, cat)
    }

    #[test]
    fn slack_aliases_come_from_the_cached_directory() {
        let (_d, cat) = open();
        let id = second_brain_kernel::AccountId::new("work-slack").unwrap();
        cat.add_account(
            &id,
            AccountKind::Slack,
            "Work",
            Some("T1:U1"),
            &json!({"x": 1}),
        )
        .unwrap();
        // No directory yet: nothing to write.
        assert_eq!(refresh_aliases(&cat).unwrap(), 0);
        cat.cache_put(
            "work-slack",
            "slack.users",
            &json!({
                "U1": {"name": "dana", "display_name": "Dana", "real_name": "Dana Example", "email": "dana@example.test"},
                "U2": {"name": "kei", "real_name": "Kei", "email": "kei@example.test"}
            }),
            chrono::Duration::seconds(60),
        )
        .unwrap();
        assert_eq!(refresh_aliases(&cat).unwrap(), 1);
        let a = cat.account_by_str("work-slack").unwrap().unwrap();
        assert_eq!(a.config["x"], 1, "other keys are kept");
        assert_eq!(
            a.config["aliases"]["names"],
            json!(["Dana Example", "Dana"])
        );
        assert_eq!(a.config["aliases"]["emails"], json!(["dana@example.test"]));
        // Unchanged data writes nothing.
        assert_eq!(refresh_aliases(&cat).unwrap(), 0);
        let all = load_aliases(&cat).unwrap();
        assert!(all.set.matches("dana example").is_some());
        assert!(
            all.set.matches("Kei").is_none(),
            "other members are not aliases"
        );
    }

    #[test]
    fn extra_aliases_and_disabled_accounts() {
        let (_d, cat) = open();
        let id = second_brain_kernel::AccountId::new("g").unwrap();
        cat.add_account(
            &id,
            AccountKind::Google,
            "G",
            Some("a@example.test"),
            &json!({"extra_aliases": {"names": ["Nick"]}}),
        )
        .unwrap();
        assert_eq!(refresh_aliases(&cat).unwrap(), 1);
        let all = load_aliases(&cat).unwrap();
        assert!(all.set.matches("nick").is_some());
        assert!(all.set.matches("A <a@example.test>").is_some());
        cat.set_account_status(&id, second_brain_kernel::AccountStatus::Disabled)
            .unwrap();
        assert!(load_aliases(&cat).unwrap().set.is_empty());
    }
}
