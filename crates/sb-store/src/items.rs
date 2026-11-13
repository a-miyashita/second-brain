//! Items and item links (ADR-0024, items.md, data-model.md).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, params, params_from_iter};
use second_brain_kernel::util::ts;
use second_brain_kernel::{
    Confidence, IncomingLink, ItemKind, LinkRelation, NewItem, SourceKind, StoredItem,
};
use serde::Serialize;

use crate::catalog::{Catalog, opt_ts, parse_col, parse_json};
use crate::entries::{ENTRY_COLS, Entry, EntryFilter};
use crate::error::Result;

/// Replace the items of an entry (an empty slice deletes them).
pub(crate) fn replace_items_tx(conn: &Connection, entry_id: i64, items: &[NewItem]) -> Result<()> {
    conn.execute("DELETE FROM items WHERE entry_id = ?1", [entry_id])?;
    let mut stmt = conn.prepare(
        "INSERT INTO items(entry_id, ordinal, kind, text, status, owner, requester, due, actors,
                           occurred_at, confidence, evidence, scope_key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?;
    for it in items {
        stmt.execute(params![
            entry_id,
            it.ordinal,
            it.kind.as_str(),
            it.text,
            it.status.map(|s| s.as_str()),
            it.owner,
            it.requester,
            it.due,
            serde_json::to_string(&it.actors)?,
            it.occurred_at.map(ts),
            it.confidence.as_str(),
            it.evidence,
            it.scope_key,
        ])?;
    }
    Ok(())
}

/// An item with its entry and the summary that produced it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemRow {
    pub stored: StoredItem,
    pub entry: Entry,
    pub model: Option<String>,
    pub profile: Option<String>,
    pub prompt_version: Option<String>,
}

/// SQL-level filters of an item query. Status, staleness and "mine" are applied
/// by the caller, because they depend on links and aliases.
#[derive(Debug, Clone, Default)]
pub struct ItemFilter {
    pub kinds: Vec<ItemKind>,
    pub accounts: Vec<String>,
    pub source_kinds: Vec<SourceKind>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub scope: Option<String>,
    /// Actions with `due` on or before this `YYYY-MM-DD`.
    pub due_by: Option<String>,
    pub min_confidence: Option<Confidence>,
    pub limit: Option<u64>,
}

/// The time an item is placed at: its own time, else its entry's activity time.
const AT: &str = "COALESCE(i.occurred_at, e.source_updated_at, e.source_created_at, e.ingested_at)";
/// The activity time of an entry.
const ACTIVITY: &str = "COALESCE(e.source_updated_at, e.source_created_at, e.ingested_at)";

const ITEM_COLS: &str = "i.id, i.entry_id, i.ordinal, i.kind, i.text, i.status, i.owner, i.requester, i.due, \
    i.actors, i.occurred_at, i.confidence, i.evidence, i.scope_key";

fn item_from_row(r: &rusqlite::Row<'_>, base: usize) -> rusqlite::Result<StoredItem> {
    let status: Option<String> = r.get(base + 5)?;
    let actors: String = r.get(base + 9)?;
    let occurred: Option<String> = r.get(base + 10)?;
    let actors: Vec<String> = parse_json(actors, base + 9)?
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(StoredItem {
        id: r.get(base)?,
        entry_id: r.get(base + 1)?,
        item: NewItem {
            ordinal: r.get(base + 2)?,
            kind: parse_col(r.get(base + 3)?, base + 3)?,
            text: r.get(base + 4)?,
            status: status.map(|s| parse_col(s, base + 5)).transpose()?,
            owner: r.get(base + 6)?,
            requester: r.get(base + 7)?,
            due: r.get(base + 8)?,
            actors,
            occurred_at: opt_ts(occurred),
            confidence: parse_col(r.get(base + 11)?, base + 11)?,
            evidence: r.get(base + 12)?,
            scope_key: r.get(base + 13)?,
        },
    })
}

fn confidences_at_least(min: Confidence) -> Vec<&'static str> {
    [Confidence::Low, Confidence::Medium, Confidence::High]
        .into_iter()
        .filter(|c| *c >= min)
        .map(|c| c.as_str())
        .collect()
}

fn push_in(conds: &mut Vec<String>, p: &mut Vec<SqlValue>, col: &str, vals: Vec<String>) {
    if vals.is_empty() {
        return;
    }
    conds.push(format!("{col} IN ({})", vec!["?"; vals.len()].join(", ")));
    p.extend(vals.into_iter().map(SqlValue::Text));
}

impl Catalog {
    /// Items matching the SQL-level filters, newest first.
    pub fn query_items(&self, f: &ItemFilter) -> Result<Vec<ItemRow>> {
        let mut conds: Vec<String> = Vec::new();
        let mut p: Vec<SqlValue> = Vec::new();
        push_in(
            &mut conds,
            &mut p,
            "i.kind",
            f.kinds.iter().map(|k| k.as_str().to_string()).collect(),
        );
        push_in(&mut conds, &mut p, "e.account_id", f.accounts.clone());
        push_in(
            &mut conds,
            &mut p,
            "e.source_kind",
            f.source_kinds.iter().map(|k| k.to_string()).collect(),
        );
        if let Some(s) = f.since {
            conds.push(format!("{AT} >= ?"));
            p.push(SqlValue::Text(ts(s)));
        }
        if let Some(u) = f.until {
            conds.push(format!("{AT} < ?"));
            p.push(SqlValue::Text(ts(u)));
        }
        if let Some(s) = &f.scope {
            conds.push("i.scope_key = ?".into());
            p.push(SqlValue::Text(s.clone()));
        }
        if let Some(d) = &f.due_by {
            conds.push("i.kind = 'action' AND i.due IS NOT NULL AND i.due <= ?".into());
            p.push(SqlValue::Text(d.clone()));
        }
        if let Some(c) = f.min_confidence {
            push_in(
                &mut conds,
                &mut p,
                "i.confidence",
                confidences_at_least(c)
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            );
        }
        let w = if conds.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conds.join(" AND "))
        };
        let mut sql = format!(
            "SELECT {ENTRY_COLS}, {ITEM_COLS}, s.model, s.profile, s.prompt_version
             FROM items i JOIN entries e ON e.id = i.entry_id
             LEFT JOIN summaries s ON s.entry_id = e.id
             {w} ORDER BY {AT} DESC, i.id DESC"
        );
        if let Some(l) = f.limit {
            sql.push_str(" LIMIT ?");
            p.push(SqlValue::Integer(l as i64));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(p), |r| {
                Ok(ItemRow {
                    entry: Entry::from_row(r)?,
                    stored: item_from_row(r, 19)?,
                    model: r.get(33)?,
                    profile: r.get(34)?,
                    prompt_version: r.get(35)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The items of one entry, in output order.
    pub fn items_for_entry(&self, entry_id: i64) -> Result<Vec<StoredItem>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ITEM_COLS} FROM items i WHERE i.entry_id = ?1 ORDER BY i.ordinal"
        ))?;
        let rows = stmt
            .query_map([entry_id], |r| item_from_row(r, 0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Items placed in `[from, to]` that can close another item, with their
    /// entry: the pool of candidate pairs. Only entries other than the target's
    /// are filtered by the caller.
    pub fn closure_pool(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<ItemRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ENTRY_COLS}, {ITEM_COLS}, s.model, s.profile, s.prompt_version
             FROM items i JOIN entries e ON e.id = i.entry_id
             LEFT JOIN summaries s ON s.entry_id = e.id
             WHERE (i.kind IN ('resolved', 'accomplishment', 'decision')
                    OR (i.kind = 'action' AND i.status IN ('done', 'dropped')))
               AND i.occurred_at >= ?1 AND i.occurred_at <= ?2"
        ))?;
        let rows = stmt
            .query_map(params![ts(from), ts(to)], |r| {
                Ok(ItemRow {
                    entry: Entry::from_row(r)?,
                    stored: item_from_row(r, 19)?,
                    model: r.get(33)?,
                    profile: r.get(34)?,
                    prompt_version: r.get(35)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Links that point at the given items, by target item id.
    pub fn incoming_links(&self, item_ids: &[i64]) -> Result<HashMap<i64, Vec<IncomingLink>>> {
        let mut out: HashMap<i64, Vec<IncomingLink>> = HashMap::new();
        for chunk in item_ids.chunks(500) {
            let ph = vec!["?"; chunk.len()].join(", ");
            let mut stmt = self.conn.prepare(&format!(
                "SELECT l.to_item, l.relation, l.confidence,
                        (SELECT f.occurred_at FROM items f WHERE f.id = l.from_item)
                 FROM item_links l WHERE l.to_item IN ({ph})"
            ))?;
            let rows = stmt
                .query_map(params_from_iter(chunk.iter()), |r| {
                    let rel: String = r.get(1)?;
                    let conf: String = r.get(2)?;
                    let at: Option<String> = r.get(3)?;
                    Ok((
                        r.get::<_, i64>(0)?,
                        rel.parse::<LinkRelation>().ok(),
                        conf.parse::<Confidence>().ok(),
                        opt_ts(at),
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (to, rel, conf, at) in rows {
                if let (Some(relation), Some(confidence)) = (rel, conf) {
                    out.entry(to).or_default().push(IncomingLink {
                        relation,
                        confidence,
                        from_occurred_at: at,
                    });
                }
            }
        }
        Ok(out)
    }

    /// The newest `occurred_at` of the items of each scope.
    pub fn scope_latest(&self, scopes: &[String]) -> Result<HashMap<String, DateTime<Utc>>> {
        let mut out = HashMap::new();
        let mut stmt = self
            .conn
            .prepare("SELECT MAX(occurred_at) FROM items WHERE scope_key = ?1")?;
        for s in scopes {
            let m: Option<String> = stmt.query_row([s], |r| r.get(0))?;
            if let Some(t) = opt_ts(m) {
                out.insert(s.clone(), t);
            }
        }
        Ok(out)
    }

    /// Entries by activity time, newest first (`sb recent`).
    pub fn recent_entries(&self, f: &RecentFilter) -> Result<Vec<Entry>> {
        let base = EntryFilter {
            accounts: f.accounts.clone(),
            source_kinds: f.source_kinds.clone(),
            ..Default::default()
        };
        let (w, mut p) = base.where_sql();
        let mut conds: Vec<String> = Vec::new();
        if let Some(s) = f.since {
            conds.push(format!("{ACTIVITY} >= ?"));
            p.push(SqlValue::Text(ts(s)));
        }
        if let Some(u) = f.until {
            conds.push(format!("{ACTIVITY} < ?"));
            p.push(SqlValue::Text(ts(u)));
        }
        let w = match (w.is_empty(), conds.is_empty()) {
            (true, true) => String::new(),
            (true, false) => format!("WHERE {}", conds.join(" AND ")),
            (false, true) => w,
            (false, false) => format!("{w} AND {}", conds.join(" AND ")),
        };
        let mut sql =
            format!("SELECT {ENTRY_COLS} FROM entries e {w} ORDER BY {ACTIVITY} DESC, e.id DESC");
        if let Some(l) = f.limit {
            sql.push_str(" LIMIT ?");
            p.push(SqlValue::Integer(l as i64));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(p), Entry::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// How many entries in the range can and cannot answer item questions.
    pub fn items_coverage(&self, f: &RecentFilter) -> Result<ItemsCoverage> {
        let base = EntryFilter {
            accounts: f.accounts.clone(),
            source_kinds: f.source_kinds.clone(),
            ..Default::default()
        };
        let (w, mut p) = base.where_sql();
        let mut conds: Vec<String> = Vec::new();
        if let Some(s) = f.since {
            conds.push(format!("{ACTIVITY} >= ?"));
            p.push(SqlValue::Text(ts(s)));
        }
        if let Some(u) = f.until {
            conds.push(format!("{ACTIVITY} < ?"));
            p.push(SqlValue::Text(ts(u)));
        }
        let w = match (w.is_empty(), conds.is_empty()) {
            (true, true) => String::new(),
            (true, false) => format!("WHERE {}", conds.join(" AND ")),
            (false, true) => w,
            (false, false) => format!("{w} AND {}", conds.join(" AND ")),
        };
        let sql = format!(
            "SELECT e.summary_status, s.generator_kind, s.prompt_version,
                    EXISTS (SELECT 1 FROM items i WHERE i.entry_id = e.id)
             FROM entries e LEFT JOIN summaries s ON s.entry_id = e.id {w}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(p), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, bool>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut c = ItemsCoverage::default();
        for (status, generator, prompt, has_items) in rows {
            c.entries_in_range += 1;
            if has_items {
                c.entries_with_items += 1;
            }
            match status.as_str() {
                "pending" | "skipped" | "failed" => c.unsummarized += 1,
                "done" => match generator.as_deref() {
                    Some("source_native") => c.native += 1,
                    _ => {
                        if !prompt
                            .as_deref()
                            .is_some_and(second_brain_kernel::prompt_extracts_items)
                        {
                            c.predate_items += 1;
                        }
                    }
                },
                _ => {}
            }
        }
        c.entries_without_items = c.unsummarized + c.predate_items + c.native;
        Ok(c)
    }
}

/// Entry-level range and scope filters of `sb recent` and of the coverage counts.
#[derive(Debug, Clone, Default)]
pub struct RecentFilter {
    pub accounts: Vec<String>,
    pub source_kinds: Vec<SourceKind>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub limit: Option<u64>,
}

/// Counts that tell how complete an item answer can be (items.md "`sb items`").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ItemsCoverage {
    pub entries_in_range: u64,
    pub entries_with_items: u64,
    /// Below the thresholds, pending or failed: no summary, so no items.
    pub unsummarized: u64,
    /// Summarized by a prompt older than the item prompts.
    pub predate_items: u64,
    /// Summarized by the source (Gemini notes): no items.
    pub native: u64,
    /// `unsummarized + predate_items + native`.
    pub entries_without_items: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home::Home;
    use crate::{EntryUpdate, NormalizedUpdate, SummaryCommit, SummaryDecision};
    use second_brain_kernel::{
        AccountId, AccountKind, EntryOrigin, Generator, GeneratorKind, ItemStatus, SectionDraft,
        SectionKind, SectionOrigin, SourceRef, Usage,
    };

    fn open() -> (tempfile::TempDir, Catalog) {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path().to_path_buf());
        let cat = Catalog::create(&home).unwrap();
        cat.add_account(
            &AccountId::new("a").unwrap(),
            AccountKind::Slack,
            "A",
            None,
            &serde_json::json!({}),
        )
        .unwrap();
        (dir, cat)
    }

    fn entry(cat: &Catalog, id: &str, at: &str) -> i64 {
        let t = opt_ts(Some(at.into()));
        let u = EntryUpdate {
            source_ref: SourceRef {
                account_id: AccountId::new("a").unwrap(),
                source_kind: SourceKind::SlackThread,
                source_id: id.into(),
                source_url: None,
                created_at: t,
                updated_at: t,
            },
            origin: EntryOrigin::Sync,
            raw: None,
            fetch_state: None,
            metadata: None,
            normalized: Some(NormalizedUpdate {
                title: id.into(),
                source_url: None,
                source_created_at: t,
                source_updated_at: t,
                sections: vec![],
                summary: SummaryDecision::Pending,
                links: vec![],
            }),
        };
        cat.upsert_entry(&u).unwrap().entry_id
    }

    fn item(ord: i64, kind: ItemKind, status: Option<ItemStatus>, text: &str, at: &str) -> NewItem {
        NewItem {
            ordinal: ord,
            kind,
            text: text.into(),
            status,
            owner: Some("Dana".into()),
            requester: None,
            due: Some("2026-11-20".into()),
            actors: vec!["Dana".into()],
            occurred_at: opt_ts(Some(at.into())),
            confidence: Confidence::High,
            evidence: Some("q".into()),
            scope_key: "slack:C1".into(),
        }
    }

    fn commit(cat: &Catalog, entry_id: i64, items: Vec<NewItem>, prompt: &str) {
        cat.commit_summary(&SummaryCommit {
            entry_id,
            sections: vec![SectionDraft {
                kind: SectionKind::Overview,
                origin: SectionOrigin::Generated,
                text: "o".into(),
            }],
            generator: Generator {
                kind: GeneratorKind::LlmApi,
                provider: "anthropic".into(),
                model: "m".into(),
                prompt_version: Some(prompt.into()),
            },
            profile: Some("fast".into()),
            input_hash: "b2:x".into(),
            usage: Usage::default(),
            cost_usd: Some(0.0),
            run_id: None,
            items,
            item_counts: Some(second_brain_kernel::ItemCounts {
                kept: 1,
                downgraded: 0,
                dropped: 2,
            }),
        })
        .unwrap();
    }

    #[test]
    fn items_are_replaced_with_the_summary_and_cascade() {
        let (_d, cat) = open();
        let e = entry(&cat, "C1:1", "2026-11-10T00:00:00Z");
        commit(
            &cat,
            e,
            vec![
                item(
                    0,
                    ItemKind::Action,
                    Some(ItemStatus::Open),
                    "a",
                    "2026-11-10T01:00:00Z",
                ),
                item(1, ItemKind::Decision, None, "b", "2026-11-10T02:00:00Z"),
            ],
            "conversation-summary/v2",
        );
        assert_eq!(cat.items_for_entry(e).unwrap().len(), 2);
        let usage = cat.summary(e).unwrap().unwrap().usage.unwrap();
        assert_eq!(usage["items"]["dropped"], 2);
        // A second commit replaces them.
        commit(
            &cat,
            e,
            vec![item(
                0,
                ItemKind::Action,
                Some(ItemStatus::Done),
                "c",
                "2026-11-10T03:00:00Z",
            )],
            "conversation-summary/v2",
        );
        let items = cat.items_for_entry(e).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item.text, "c");
        assert_eq!(items[0].item.status, Some(ItemStatus::Done));
        assert_eq!(items[0].item.actors, vec!["Dana".to_string()]);
        // Deleting the entry removes its items.
        cat.delete_entry(e).unwrap();
        assert!(cat.items_for_entry(e).unwrap().is_empty());
    }

    #[test]
    fn queries_filter_and_join_provenance() {
        let (_d, cat) = open();
        let e1 = entry(&cat, "C1:1", "2026-11-10T00:00:00Z");
        let e2 = entry(&cat, "C1:2", "2026-11-12T00:00:00Z");
        commit(
            &cat,
            e1,
            vec![item(
                0,
                ItemKind::Action,
                Some(ItemStatus::Open),
                "old",
                "2026-11-10T01:00:00Z",
            )],
            "conversation-summary/v2",
        );
        let mut low = item(0, ItemKind::Decision, None, "weak", "2026-11-12T01:00:00Z");
        low.confidence = Confidence::Low;
        low.due = None;
        commit(&cat, e2, vec![low], "conversation-summary/v2");
        let all = cat.query_items(&ItemFilter::default()).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].stored.item.text, "weak", "newest first");
        assert_eq!(all[0].model.as_deref(), Some("m"));
        assert_eq!(
            all[0].prompt_version.as_deref(),
            Some("conversation-summary/v2")
        );
        let f = ItemFilter {
            kinds: vec![ItemKind::Action],
            ..Default::default()
        };
        assert_eq!(cat.query_items(&f).unwrap().len(), 1);
        let f = ItemFilter {
            min_confidence: Some(Confidence::Medium),
            ..Default::default()
        };
        assert_eq!(cat.query_items(&f).unwrap().len(), 1);
        let f = ItemFilter {
            since: opt_ts(Some("2026-11-11T00:00:00Z".into())),
            ..Default::default()
        };
        assert_eq!(cat.query_items(&f).unwrap().len(), 1);
        let f = ItemFilter {
            due_by: Some("2026-11-20".into()),
            ..Default::default()
        };
        assert_eq!(cat.query_items(&f).unwrap().len(), 1);
        let latest = cat
            .scope_latest(&["slack:C1".into(), "none".into()])
            .unwrap();
        assert_eq!(latest.len(), 1);
    }

    #[test]
    fn coverage_counts_entries_that_cannot_answer() {
        let (_d, cat) = open();
        let with = entry(&cat, "C1:1", "2026-11-10T00:00:00Z");
        let old = entry(&cat, "C1:2", "2026-11-10T00:00:00Z");
        let _pending = entry(&cat, "C1:3", "2026-11-10T00:00:00Z");
        commit(
            &cat,
            with,
            vec![item(
                0,
                ItemKind::Decision,
                None,
                "d",
                "2026-11-10T01:00:00Z",
            )],
            "conversation-summary/v2",
        );
        commit(&cat, old, vec![], "conversation-summary/v1");
        let c = cat.items_coverage(&RecentFilter::default()).unwrap();
        assert_eq!(c.entries_in_range, 3);
        assert_eq!(c.entries_with_items, 1);
        assert_eq!(c.unsummarized, 1);
        assert_eq!(c.predate_items, 1);
        assert_eq!(c.entries_without_items, 2);
        let recent = cat
            .recent_entries(&RecentFilter {
                since: opt_ts(Some("2026-11-11T00:00:00Z".into())),
                ..Default::default()
            })
            .unwrap();
        assert!(recent.is_empty());
    }
}
