//! How far back each account's synced data reaches (ADR-0016), for `sb stats`
//! and the JSON of `sb sync`.

use chrono::{DateTime, TimeZone, Utc};
use second_brain_kernel::coverage::covered_since_of;
use second_brain_kernel::util::{parse_ts, ts};
use second_brain_kernel::{AccountId, SourceKind};
use second_brain_store::Catalog;
use serde::Serialize;
use serde_json::Value;

/// The interval an account's source has fetched. `covered_since` is the start
/// that holds for every scope (the latest one), and `covered_until` the oldest
/// forward cursor. `None` means unknown.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoverageRow {
    pub account: String,
    pub source: String,
    pub covered_since: Option<String>,
    pub covered_until: Option<String>,
}

/// Slack stores `oldest` as a `seconds.micros` string.
fn slack_time(v: &Value) -> Option<DateTime<Utc>> {
    let secs: f64 = v.get("oldest")?.as_str()?.parse().ok()?;
    Utc.timestamp_opt(secs as i64, 0).single()
}

fn rfc3339_field(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    v.get(key)?.as_str().and_then(parse_ts)
}

/// `(covered_since, covered_until)` of one scope; `None` is unknown.
type Scope = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);

/// The `(covered_since, covered_until)` of a set of scopes.
fn fold(scopes: &[Scope]) -> (Option<String>, Option<String>) {
    let since = scopes.iter().filter_map(|s| s.0).max();
    let until = scopes.iter().filter_map(|s| s.1).min();
    (since.map(ts), until.map(ts))
}

/// Coverage per account and source, for the accounts that have cursors.
pub fn rows(cat: &Catalog) -> anyhow::Result<Vec<CoverageRow>> {
    let mut out = Vec::new();
    for a in cat.accounts()? {
        let id: &AccountId = &a.id;
        let slack = cat.cursors(id, SourceKind::SlackThread, "conv:")?;
        if !slack.is_empty() {
            let scopes: Vec<_> = slack
                .iter()
                .map(|(_, v)| (covered_since_of(v), slack_time(v)))
                .collect();
            let (since, until) = fold(&scopes);
            out.push(CoverageRow {
                account: id.to_string(),
                source: "slack".into(),
                covered_since: since,
                covered_until: until,
            });
        }
        let mut meet = Vec::new();
        for (key, field) in [("calendar", "last_time_max"), ("drive", "modified_after")] {
            if let Some(v) = cat.cursor(id, SourceKind::GoogleMeet, key)? {
                meet.push((covered_since_of(&v), rfc3339_field(&v, field)));
            }
        }
        if !meet.is_empty() {
            let (since, until) = fold(&meet);
            out.push(CoverageRow {
                account: id.to_string(),
                source: SourceKind::GoogleMeet.as_str().into(),
                covered_since: since,
                covered_until: until,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Option<DateTime<Utc>> {
        parse_ts(s)
    }

    #[test]
    fn the_guaranteed_start_is_the_latest_and_the_end_the_earliest() {
        let (since, until) = fold(&[
            (t("2026-08-01T00:00:00Z"), t("2026-09-10T00:00:00Z")),
            (t("2026-08-11T00:00:00Z"), t("2026-09-09T00:00:00Z")),
            (None, t("2026-09-11T00:00:00Z")),
        ]);
        assert_eq!(since.as_deref(), Some("2026-08-11T00:00:00Z"));
        assert_eq!(until.as_deref(), Some("2026-09-09T00:00:00Z"));
    }

    #[test]
    fn slack_cursors_are_parsed() {
        let v = serde_json::json!({"oldest": "1788998400.000000"});
        assert_eq!(slack_time(&v), t("2026-09-10T00:00:00Z"));
    }
}
