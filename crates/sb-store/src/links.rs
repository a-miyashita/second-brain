//! Document links found in Slack entries and their ingest state
//! (docs/specs/link-ingest.md, ADR-0022).

use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, params};
use second_brain_kernel::links::DiscoveredLink;
use second_brain_kernel::util::ts;
use second_brain_kernel::{LinkStatus, SourceKind};
use serde::Serialize;

use crate::catalog::{Catalog, OptionalExt, opt_ts, parse_col, req_ts};
use crate::error::Result;

/// A row of `discovered_links`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkRow {
    pub account_id: String,
    pub link_key: String,
    pub source_kind: SourceKind,
    /// May carry a sharing token: never log it.
    pub url: String,
    pub host: String,
    pub first_entry_id: Option<i64>,
    pub first_seen_at: DateTime<Utc>,
    pub excerpt: Option<String>,
    pub status: LinkStatus,
    pub entry_id: Option<i64>,
    pub attempts: i64,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

/// What the link stage learned about one link.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkOutcome {
    /// An entry exists for the document.
    Ingested { entry_id: i64 },
    /// The document is already known under another kind (a `google.meet` entry).
    Duplicate { entry_id: i64 },
    /// Not something to make an entry from (a folder, an unsupported type, ...).
    NotApplicable { reason: String },
    /// No eligible account can read it. Counts as a failure and backs off.
    Unavailable { error: String, max_attempts: i64 },
    /// Try again in the next run; not counted (network, rate limit, no account,
    /// a limit that stopped the run).
    Waiting { error: String },
}

/// Delay before the next try after the `attempts`-th failure (1-based):
/// 1, 3, 7, 14, then 30 days.
pub fn link_backoff(attempts: i64) -> Duration {
    Duration::days(match attempts {
        ..=1 => 1,
        2 => 3,
        3 => 7,
        4 => 14,
        _ => 30,
    })
}

/// Record the links of an entry. The first reference wins, so an existing row
/// is left as it is. Runs in the entry's transaction.
pub(crate) fn record_links_tx(
    conn: &Connection,
    now: &str,
    account_id: &str,
    entry_id: i64,
    links: &[DiscoveredLink],
) -> Result<()> {
    for l in links {
        conn.execute(
            "INSERT INTO discovered_links(account_id, link_key, source_kind, url, host, first_entry_id,
               first_seen_at, excerpt, status, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?7)
             ON CONFLICT(account_id, link_key) DO NOTHING",
            params![
                account_id,
                l.link.key,
                l.link.kind.as_str(),
                l.link.url,
                l.link.host,
                entry_id,
                now,
                l.excerpt,
            ],
        )?;
    }
    Ok(())
}

const COLS: &str =
    "account_id, link_key, source_kind, url, host, first_entry_id, first_seen_at, excerpt,
                    status, entry_id, attempts, next_attempt_at, last_error";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<LinkRow> {
    Ok(LinkRow {
        account_id: r.get(0)?,
        link_key: r.get(1)?,
        source_kind: parse_col(r.get(2)?, 2)?,
        url: r.get(3)?,
        host: r.get(4)?,
        first_entry_id: r.get(5)?,
        first_seen_at: req_ts(r.get(6)?),
        excerpt: r.get(7)?,
        status: parse_col(r.get(8)?, 8)?,
        entry_id: r.get(9)?,
        attempts: r.get(10)?,
        next_attempt_at: opt_ts(r.get(11)?),
        last_error: r.get(12)?,
    })
}

impl Catalog {
    /// Links of a Slack account that are due: `pending` ones that may be tried
    /// now, and `unavailable` ones whose delay has passed. Newest first.
    pub fn due_links(
        &self,
        account_id: &str,
        kinds: &[SourceKind],
        now: DateTime<Utc>,
        limit: u64,
    ) -> Result<Vec<LinkRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLS} FROM discovered_links
             WHERE account_id = ?1
               AND ((status = 'pending' AND (next_attempt_at IS NULL OR next_attempt_at <= ?2))
                    OR (status = 'unavailable' AND next_attempt_at <= ?2))
             ORDER BY first_seen_at DESC, link_key"
        ))?;
        let now_s = ts(now);
        let rows = stmt
            .query_map(params![account_id, now_s], row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .filter(|l| kinds.contains(&l.source_kind))
            .take(limit as usize)
            .collect())
    }

    /// One link by key.
    pub fn link(&self, account_id: &str, link_key: &str) -> Result<Option<LinkRow>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {COLS} FROM discovered_links WHERE account_id = ?1 AND link_key = ?2"
                ),
                params![account_id, link_key],
                row,
            )
            .opt()
    }

    /// Links of the account(s) for listing, newest first.
    pub fn list_links(
        &self,
        account_id: Option<&str>,
        kinds: &[SourceKind],
        limit: u64,
    ) -> Result<Vec<LinkRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLS} FROM discovered_links
             WHERE (?1 IS NULL OR account_id = ?1)
             ORDER BY first_seen_at DESC, link_key"
        ))?;
        let rows = stmt
            .query_map(params![account_id], row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .filter(|l| kinds.is_empty() || kinds.contains(&l.source_kind))
            .take(limit as usize)
            .collect())
    }

    /// Record the result for a link (in its own transaction, or use
    /// [`record_link_outcome_tx`] inside the commit of the entry).
    pub fn record_link_outcome(
        &self,
        account_id: &str,
        link_key: &str,
        outcome: &LinkOutcome,
    ) -> Result<()> {
        let now = self.clock.now();
        self.with_tx(|tx| record_link_outcome_tx(tx, now, account_id, link_key, outcome))
    }

    /// Make links due again: `unavailable` ones (delay ignored) and `abandoned`
    /// ones (attempts reset) go back to `pending`. Returns the number reset.
    pub fn retry_links(&self, account_id: Option<&str>, kinds: &[SourceKind]) -> Result<u64> {
        let now = self.now_ts();
        let rows = self.list_links(account_id, kinds, u64::MAX)?;
        let mut n = 0;
        for l in rows
            .iter()
            .filter(|l| matches!(l.status, LinkStatus::Unavailable | LinkStatus::Abandoned))
        {
            n += self.conn.execute(
                "UPDATE discovered_links SET status = 'pending', attempts = 0, next_attempt_at = NULL,
                   updated_at = ?3 WHERE account_id = ?1 AND link_key = ?2",
                params![l.account_id, l.link_key, now],
            )? as u64;
        }
        Ok(n)
    }

    /// Number of links per status, optionally for one account.
    pub fn link_counts(&self, account_id: Option<&str>) -> Result<Vec<(LinkStatus, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT status, count(*) FROM discovered_links
             WHERE (?1 IS NULL OR account_id = ?1) GROUP BY status ORDER BY status",
        )?;
        let rows = stmt
            .query_map(params![account_id], |r| {
                Ok((parse_col::<LinkStatus>(r.get(0)?, 0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

/// Record a link result inside a transaction (the caller commits).
pub fn record_link_outcome_tx(
    conn: &Connection,
    now: DateTime<Utc>,
    account_id: &str,
    link_key: &str,
    outcome: &LinkOutcome,
) -> Result<()> {
    let now_s = ts(now);
    match outcome {
        LinkOutcome::Ingested { entry_id } => conn.execute(
            "UPDATE discovered_links SET status = 'ingested', entry_id = ?3, next_attempt_at = NULL,
               last_error = NULL, updated_at = ?4 WHERE account_id = ?1 AND link_key = ?2",
            params![account_id, link_key, entry_id, now_s],
        )?,
        LinkOutcome::Duplicate { entry_id } => conn.execute(
            "UPDATE discovered_links SET status = 'duplicate', entry_id = ?3, next_attempt_at = NULL,
               last_error = NULL, updated_at = ?4 WHERE account_id = ?1 AND link_key = ?2",
            params![account_id, link_key, entry_id, now_s],
        )?,
        LinkOutcome::NotApplicable { reason } => conn.execute(
            "UPDATE discovered_links SET status = 'not_applicable', next_attempt_at = NULL,
               last_error = ?3, updated_at = ?4 WHERE account_id = ?1 AND link_key = ?2",
            params![account_id, link_key, reason, now_s],
        )?,
        LinkOutcome::Waiting { error } => conn.execute(
            "UPDATE discovered_links SET last_error = ?3, updated_at = ?4
             WHERE account_id = ?1 AND link_key = ?2",
            params![account_id, link_key, error, now_s],
        )?,
        LinkOutcome::Unavailable { error, max_attempts } => {
            let attempts: i64 = conn
                .query_row(
                    "SELECT attempts FROM discovered_links WHERE account_id = ?1 AND link_key = ?2",
                    params![account_id, link_key],
                    |r| r.get(0),
                )
                .opt()?
                .unwrap_or(0)
                + 1;
            if attempts >= *max_attempts {
                conn.execute(
                    "UPDATE discovered_links SET status = 'abandoned', attempts = ?3, next_attempt_at = NULL,
                       last_error = ?4, updated_at = ?5 WHERE account_id = ?1 AND link_key = ?2",
                    params![account_id, link_key, attempts, error, now_s],
                )?
            } else {
                conn.execute(
                    "UPDATE discovered_links SET status = 'unavailable', attempts = ?3, next_attempt_at = ?4,
                       last_error = ?5, updated_at = ?6 WHERE account_id = ?1 AND link_key = ?2",
                    params![
                        account_id,
                        link_key,
                        attempts,
                        ts(now + link_backoff(attempts)),
                        error,
                        now_s
                    ],
                )?
            }
        }
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::test_util::temp_catalog;
    use crate::entries::{EntryUpdate, NormalizedUpdate, SummaryDecision};
    use second_brain_kernel::links::{LinkRef, classify_link};
    use second_brain_kernel::{AccountId, AccountKind, EntryOrigin, SourceRef};

    fn link(url: &str, excerpt: &str) -> DiscoveredLink {
        let l: LinkRef = classify_link(url).unwrap();
        DiscoveredLink {
            link: l,
            message_ts: "1.0".into(),
            excerpt: excerpt.into(),
        }
    }

    fn slack_entry(cat: &Catalog, id: &str, links: Vec<DiscoveredLink>) -> i64 {
        cat.upsert_entry(&EntryUpdate {
            source_ref: SourceRef {
                account_id: AccountId::new("acme").unwrap(),
                source_kind: SourceKind::SlackThread,
                source_id: id.into(),
                source_url: None,
                created_at: None,
                updated_at: None,
            },
            origin: EntryOrigin::Sync,
            raw: None,
            fetch_state: None,
            metadata: None,
            normalized: Some(NormalizedUpdate {
                title: "t".into(),
                source_url: None,
                source_created_at: None,
                source_updated_at: None,
                sections: vec![],
                summary: SummaryDecision::NoSummary,
                links,
            }),
        })
        .unwrap()
        .entry_id
    }

    fn setup() -> (tempfile::TempDir, Catalog) {
        let (d, cat) = temp_catalog();
        cat.ensure_account(&AccountId::new("acme").unwrap(), AccountKind::Slack, "Acme")
            .unwrap();
        (d, cat)
    }

    const DOC: &str = "https://docs.google.com/document/d/AAA/edit";
    const DOC_OTHER_FORM: &str = "https://drive.google.com/open?id=AAA";
    const SP: &str = "https://contoso.sharepoint.com/:w:/s/team/Eabc";

    #[test]
    fn links_are_recorded_with_the_entry_and_the_first_reference_wins() {
        let (_d, cat) = setup();
        let e1 = slack_entry(&cat, "C1:1.0", vec![link(DOC, "first"), link(SP, "sp")]);
        let e2 = slack_entry(&cat, "C1:2.0", vec![link(DOC_OTHER_FORM, "second")]);
        assert_ne!(e1, e2);
        let all = cat.list_links(Some("acme"), &[], 100).unwrap();
        assert_eq!(all.len(), 2, "one Google file is one link: {all:?}");
        let g = cat.link("acme", "google:AAA").unwrap().unwrap();
        assert_eq!(g.excerpt.as_deref(), Some("first"));
        assert_eq!(g.first_entry_id, Some(e1));
        assert_eq!(g.status, LinkStatus::Pending);
        // Recording the same entry again changes nothing.
        slack_entry(&cat, "C1:1.0", vec![link(DOC, "changed")]);
        assert_eq!(
            cat.link("acme", "google:AAA")
                .unwrap()
                .unwrap()
                .excerpt
                .as_deref(),
            Some("first")
        );
        // Deleting the Slack entry keeps the link, without the reference.
        cat.delete_entry(e1).unwrap();
        let g = cat.link("acme", "google:AAA").unwrap().unwrap();
        assert_eq!(g.first_entry_id, None);
        assert_eq!(g.excerpt.as_deref(), Some("first"));
    }

    #[test]
    fn due_links_respect_kinds_delays_and_order() {
        let (_d, cat) = setup();
        slack_entry(&cat, "C1:1.0", vec![link(DOC, "g"), link(SP, "s")]);
        let now = cat.clock.now();
        let google = [SourceKind::GoogleDoc];
        let both = [SourceKind::GoogleDoc, SourceKind::MicrosoftFile];
        assert_eq!(cat.due_links("acme", &google, now, 10).unwrap().len(), 1);
        assert_eq!(cat.due_links("acme", &both, now, 10).unwrap().len(), 2);
        assert_eq!(cat.due_links("acme", &both, now, 1).unwrap().len(), 1);
        assert!(cat.due_links("other", &both, now, 10).unwrap().is_empty());

        // Unavailable: not due until the delay has passed.
        let out = LinkOutcome::Unavailable {
            error: "not found or no access".into(),
            max_attempts: 3,
        };
        cat.record_link_outcome("acme", "google:AAA", &out).unwrap();
        let l = cat.link("acme", "google:AAA").unwrap().unwrap();
        assert_eq!((l.status, l.attempts), (LinkStatus::Unavailable, 1));
        assert_eq!(cat.due_links("acme", &google, now, 10).unwrap().len(), 0);
        let later = now + Duration::days(2);
        assert_eq!(cat.due_links("acme", &google, later, 10).unwrap().len(), 1);
    }

    #[test]
    fn failures_back_off_and_end_as_abandoned_until_retried() {
        let (_d, cat) = setup();
        slack_entry(&cat, "C1:1.0", vec![link(DOC, "g")]);
        let out = LinkOutcome::Unavailable {
            error: "no access".into(),
            max_attempts: 3,
        };
        for _ in 0..3 {
            cat.record_link_outcome("acme", "google:AAA", &out).unwrap();
        }
        let l = cat.link("acme", "google:AAA").unwrap().unwrap();
        assert_eq!((l.status, l.attempts), (LinkStatus::Abandoned, 3));
        assert!(l.next_attempt_at.is_none());
        let far = cat.clock.now() + Duration::days(400);
        assert!(
            cat.due_links("acme", &[SourceKind::GoogleDoc], far, 10)
                .unwrap()
                .is_empty()
        );

        assert_eq!(cat.retry_links(Some("acme"), &[]).unwrap(), 1);
        let l = cat.link("acme", "google:AAA").unwrap().unwrap();
        assert_eq!((l.status, l.attempts), (LinkStatus::Pending, 0));
        assert_eq!(
            cat.due_links("acme", &[SourceKind::GoogleDoc], cat.clock.now(), 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn waiting_does_not_count_and_final_outcomes_are_recorded() {
        let (_d, cat) = setup();
        let e = slack_entry(&cat, "C1:1.0", vec![link(DOC, "g"), link(SP, "s")]);
        cat.record_link_outcome(
            "acme",
            "google:AAA",
            &LinkOutcome::Waiting {
                error: "no Google account".into(),
            },
        )
        .unwrap();
        let l = cat.link("acme", "google:AAA").unwrap().unwrap();
        assert_eq!((l.status, l.attempts), (LinkStatus::Pending, 0));
        assert_eq!(l.last_error.as_deref(), Some("no Google account"));

        cat.record_link_outcome("acme", "google:AAA", &LinkOutcome::Ingested { entry_id: e })
            .unwrap();
        let l = cat.link("acme", "google:AAA").unwrap().unwrap();
        assert_eq!(
            (l.status, l.entry_id, l.last_error),
            (LinkStatus::Ingested, Some(e), None)
        );

        let sp_key = classify_link(SP).unwrap().key;
        cat.record_link_outcome(
            "acme",
            &sp_key,
            &LinkOutcome::NotApplicable {
                reason: "folders are not supported".into(),
            },
        )
        .unwrap();
        let counts = cat.link_counts(Some("acme")).unwrap();
        assert_eq!(
            counts,
            vec![(LinkStatus::Ingested, 1), (LinkStatus::NotApplicable, 1)]
        );
    }

    #[test]
    fn backoff_schedule() {
        let days: Vec<i64> = (1..=6).map(|n| link_backoff(n).num_days()).collect();
        assert_eq!(days, vec![1, 3, 7, 14, 30, 30]);
    }

    #[test]
    fn removing_the_account_removes_its_links() {
        let (_d, cat) = setup();
        slack_entry(&cat, "C1:1.0", vec![link(DOC, "g")]);
        cat.remove_account(&AccountId::new("acme").unwrap(), true)
            .unwrap();
        assert!(cat.list_links(None, &[], 10).unwrap().is_empty());
    }
}
