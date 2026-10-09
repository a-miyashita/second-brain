//! The per-account host given to sources, and the shared commit path:
//! write raw → normalize → upsert entry, sections, queue and cursors in one
//! transaction (ADR-0012).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use sb_store::rawstore;
use sb_store::{Account, CommitBatch, Entry, EntryUpdate, NormalizedUpdate, StoredRaw};
use second_brain_kernel::source::{Source, SourceError, SyncHost};
use second_brain_kernel::{
    CursorUpdate, DiscoveryBatch, EntryOrigin, FetchedEntry, Language, NormalizeCtx,
    NormalizeInput, NormalizeOutcome, QueueItem, RawMode, RawSegment, SourceKind, SourceRef,
};
use serde_json::Value;

use crate::Pipeline;
use crate::error::PipelineError;
use crate::policy::SummaryPolicy;
use crate::run::SourceStats;

/// Merge two JSON objects; keys of `overlay` win. Non-objects are replaced.
pub(crate) fn merge_json(base: &Value, overlay: &Value) -> Value {
    match (base, overlay) {
        (Value::Object(b), Value::Object(o)) => {
            let mut m = b.clone();
            for (k, v) in o {
                m.insert(k.clone(), v.clone());
            }
            Value::Object(m)
        }
        (b, Value::Null) => b.clone(),
        (_, o) => o.clone(),
    }
}

/// One item to commit, with the queue row it settles (if any).
pub(crate) struct Item {
    pub fetched: FetchedEntry,
    pub dequeue: Option<(SourceKind, String)>,
}

/// Host for one account's source.
pub(crate) struct Host<'a> {
    pub p: &'a Pipeline,
    pub account: Account,
    pub source: Arc<dyn Source>,
    pub policy: &'a SummaryPolicy,
    pub origin: EntryOrigin,
    language: Language,
    timezone: String,
    snapshot: Mutex<Option<Value>>,
    pub stats: Mutex<BTreeMap<SourceKind, SourceStats>>,
    pub errors: Mutex<Vec<String>>,
}

fn host_err(e: impl std::fmt::Display) -> SourceError {
    SourceError::Host(e.to_string())
}

impl<'a> Host<'a> {
    pub fn new(
        p: &'a Pipeline,
        account: Account,
        source: Arc<dyn Source>,
        policy: &'a SummaryPolicy,
        origin: EntryOrigin,
    ) -> Result<Self, PipelineError> {
        let (language, timezone) = {
            let cat = p.catalog();
            let lang: String = cat.setting_or("display.language", "en".to_string())?;
            let tz: String = cat.setting_or("slack.day_timezone", "UTC".to_string())?;
            (lang.parse().unwrap_or(Language::En), tz)
        };
        Ok(Host {
            p,
            account,
            source,
            policy,
            origin,
            language,
            timezone,
            snapshot: Mutex::new(None),
            stats: Mutex::new(BTreeMap::new()),
            errors: Mutex::new(Vec::new()),
        })
    }

    pub fn stat(&self, kind: SourceKind, f: impl FnOnce(&mut SourceStats)) {
        if let Ok(mut s) = self.stats.lock() {
            f(s.entry(kind).or_default());
        }
    }

    pub fn error(&self, msg: String) {
        tracing::warn!(account = %self.account.id, "{msg}");
        if let Ok(mut e) = self.errors.lock() {
            e.push(msg);
        }
    }

    fn normalize_ctx(&self) -> Result<NormalizeCtx, PipelineError> {
        let snapshot = {
            let mut g = self
                .snapshot
                .lock()
                .map_err(|_| PipelineError::Invalid("poisoned".into()))?;
            if g.is_none() {
                *g = Some(self.source.load_snapshot(self)?);
            }
            g.clone().unwrap_or(Value::Null)
        };
        Ok(NormalizeCtx {
            account: self.account.ctx(),
            language: self.language,
            timezone: self.timezone.clone(),
            snapshot,
        })
    }

    /// Normalize an entry from its stored raw data (no network).
    pub fn normalize_stored(&self, entry: &Entry) -> Result<NormalizeOutcome, PipelineError> {
        let rows = self.p.catalog().raw_objects(entry.id)?;
        let segments = rawstore::read_segments(self.p.home(), &rows)?;
        let input = NormalizeInput {
            source_ref: entry.source_ref()?,
            fetch_metadata: entry.metadata.clone(),
            segments,
        };
        Ok(self.source.normalize(&self.normalize_ctx()?, &input)?)
    }

    /// Build the update for a re-normalized stored entry.
    pub fn renormalized_update(
        &self,
        entry: &Entry,
        outcome: NormalizeOutcome,
    ) -> Result<Option<EntryUpdate>, PipelineError> {
        let NormalizeOutcome::Entry(n) = outcome else {
            return Ok(None);
        };
        let summary = self.p.catalog().summary(entry.id)?;
        let decision = self
            .policy
            .decide(entry.source_kind, &n, Some((entry, summary.as_ref())));
        let metadata = merge_json(&entry.metadata, &n.metadata);
        Ok(Some(EntryUpdate {
            source_ref: entry.source_ref()?,
            origin: entry.origin,
            raw: None,
            fetch_state: None,
            metadata: Some(metadata),
            normalized: Some(NormalizedUpdate {
                title: n.title.clone(),
                source_url: n.source_url.clone(),
                source_created_at: n.source_created_at,
                source_updated_at: n.source_updated_at,
                sections: n.sections.clone(),
                summary: decision,
            }),
        }))
    }

    /// Normalize a fetched bundle together with the stored segments, then write
    /// its raw files. Returns `None` when the item is not applicable.
    fn prepare(
        &self,
        fe: FetchedEntry,
        now: DateTime<Utc>,
    ) -> Result<Option<(EntryUpdate, bool)>, PipelineError> {
        let r = &fe.source_ref;
        let (existing, rows, summary) = {
            let cat = self.p.catalog();
            let existing = cat.entry_by_key(r.account_id.as_str(), r.source_kind, &r.source_id)?;
            let rows = match &existing {
                Some(e) => cat.raw_objects(e.id)?,
                None => vec![],
            };
            let summary = match &existing {
                Some(e) => cat.summary(e.id)?,
                None => None,
            };
            (existing, rows, summary)
        };
        let bundle = &fe.bundle;
        let append = bundle.mode == RawMode::Append && !rows.is_empty();
        let mut segments: Vec<RawSegment> = if append {
            rawstore::read_segments(self.p.home(), &rows)?
        } else {
            vec![]
        };
        let mut seqs = Vec::with_capacity(bundle.objects.len());
        let mut next: BTreeMap<&str, i64> = BTreeMap::new();
        for o in &bundle.objects {
            let seq = if append {
                let n = next
                    .entry(o.role.as_str())
                    .or_insert_with(|| rawstore::next_seq(&rows, o.role));
                let s = *n;
                *n += 1;
                s
            } else {
                let n = next.entry(o.role.as_str()).or_insert(0);
                let s = *n;
                *n += 1;
                s
            };
            seqs.push(seq);
            segments.push(RawSegment {
                role: o.role,
                seq,
                media_type: o.media_type.clone(),
                bytes: o.bytes.clone(),
            });
        }
        segments.sort_by(|a, b| (a.role.as_str(), a.seq).cmp(&(b.role.as_str(), b.seq)));
        let base_meta = existing
            .as_ref()
            .map(|e| e.metadata.clone())
            .unwrap_or(Value::Object(Default::default()));
        let fetch_meta = merge_json(&base_meta, &bundle.metadata);
        let mut source_ref = fe.source_ref.clone();
        if source_ref.source_url.is_none() {
            source_ref.source_url = existing.as_ref().and_then(|e| e.source_url.clone());
        }
        let input = NormalizeInput {
            source_ref: source_ref.clone(),
            fetch_metadata: fetch_meta.clone(),
            segments,
        };
        let outcome = self.source.normalize(&self.normalize_ctx()?, &input)?;
        let n = match outcome {
            NormalizeOutcome::Entry(n) => n,
            NormalizeOutcome::NotApplicable(why) => {
                tracing::debug!(source_id = %r.source_id, reason = %why, "not applicable");
                return Ok(None);
            }
        };
        // Write raw files only after normalization succeeded.
        let dir = rows
            .first()
            .map(|row| rawstore::dir_of(&row.path))
            .unwrap_or_else(|| {
                rawstore::entry_dir(
                    &SourceRef {
                        created_at: n.source_created_at.or(r.created_at),
                        ..r.clone()
                    },
                    now,
                )
            });
        let mut stored: Vec<StoredRaw> = Vec::with_capacity(bundle.objects.len());
        for (o, seq) in bundle.objects.iter().zip(seqs) {
            stored.push(rawstore::write_object(self.p.home(), &dir, seq, o)?);
        }
        let decision = self.policy.decide(
            r.source_kind,
            &n,
            existing.as_ref().map(|e| (e, summary.as_ref())),
        );
        let metadata = merge_json(&fetch_meta, &n.metadata);
        let update = EntryUpdate {
            source_ref,
            origin: existing.as_ref().map(|e| e.origin).unwrap_or(self.origin),
            raw: Some((
                if append {
                    RawMode::Append
                } else {
                    RawMode::Replace
                },
                stored,
            )),
            fetch_state: bundle.fetch_state.clone(),
            metadata: Some(metadata),
            normalized: Some(NormalizedUpdate {
                title: n.title.clone(),
                source_url: n.source_url.clone(),
                source_created_at: n.source_created_at,
                source_updated_at: n.source_updated_at,
                sections: n.sections.clone(),
                summary: decision,
            }),
        };
        Ok(Some((update, existing.is_some())))
    }

    /// Commit fetched items with queue changes and cursors in one transaction.
    /// Items that fail to normalize stay queued (with a recorded failure).
    pub fn commit_items(
        &self,
        items: Vec<Item>,
        enqueue: Vec<QueueItem>,
        cursors: Vec<CursorUpdate>,
        extra_dequeue: Vec<(SourceKind, String)>,
    ) -> Result<(), PipelineError> {
        let now = self.p.clock().now();
        let mut batch = CommitBatch {
            account_id: Some(self.account.id.clone()),
            enqueue,
            cursors,
            dequeue: extra_dequeue,
            ..Default::default()
        };
        let mut kinds_existed = Vec::new();
        let mut failed_items = Vec::new();
        for item in items {
            let kind = item.fetched.source_ref.source_kind;
            let id = item.fetched.source_ref.source_id.clone();
            match self.prepare(item.fetched, now) {
                Ok(Some((u, existed))) => {
                    batch.entries.push(u);
                    kinds_existed.push((kind, existed));
                    if let Some(d) = item.dequeue {
                        batch.dequeue.push(d);
                    }
                }
                Ok(None) => {
                    self.stat(kind, |s| s.not_applicable += 1);
                    if let Some(d) = item.dequeue {
                        batch.dequeue.push(d);
                    }
                }
                Err(e) => {
                    self.stat(kind, |s| s.failed += 1);
                    self.error(format!("{kind} {id}: {e}"));
                    failed_items.push((kind, id, e.to_string(), item.dequeue.is_some()));
                }
            }
        }
        let results = self.p.catalog().commit_batch(&batch)?;
        for ((kind, existed), res) in kinds_existed.iter().zip(&results) {
            self.stat(
                *kind,
                |s| if *existed { s.updated += 1 } else { s.new += 1 },
            );
            if !res.stale_paths.is_empty() {
                rawstore::delete_paths(self.p.home(), &res.stale_paths);
            }
        }
        let cat = self.p.catalog();
        for (kind, id, err, queued) in failed_items {
            if queued {
                cat.queue_failure(&self.account.id, kind, &id, &err)?;
            }
        }
        Ok(())
    }
}

impl SyncHost for Host<'_> {
    fn cursor(&self, kind: SourceKind, key: &str) -> Result<Option<Value>, SourceError> {
        self.p
            .catalog()
            .cursor(&self.account.id, kind, key)
            .map_err(host_err)
    }

    fn cursors(&self, kind: SourceKind, prefix: &str) -> Result<Vec<(String, Value)>, SourceError> {
        self.p
            .catalog()
            .cursors(&self.account.id, kind, prefix)
            .map_err(host_err)
    }

    fn fetch_state(&self, kind: SourceKind, source_id: &str) -> Result<Option<Value>, SourceError> {
        Ok(self
            .p
            .catalog()
            .entry_by_key(self.account.id.as_str(), kind, source_id)
            .map_err(host_err)?
            .and_then(|e| e.fetch_state))
    }

    fn entry_exists(&self, kind: SourceKind, source_id: &str) -> Result<bool, SourceError> {
        Ok(self
            .p
            .catalog()
            .entry_by_key(self.account.id.as_str(), kind, source_id)
            .map_err(host_err)?
            .is_some())
    }

    fn commit(&self, batch: DiscoveryBatch) -> Result<(), SourceError> {
        for q in &batch.enqueue {
            self.stat(q.source_kind, |s| s.queued += 1);
        }
        let items = batch
            .entries
            .into_iter()
            .map(|fetched| Item {
                fetched,
                dequeue: None,
            })
            .collect();
        self.commit_items(items, batch.enqueue, batch.cursors, vec![])
            .map_err(host_err)
    }

    fn cache_get(&self, key: &str) -> Result<Option<Value>, SourceError> {
        self.p
            .catalog()
            .cache_get(self.account.id.as_str(), key)
            .map_err(host_err)
    }

    fn cache_get_stale(&self, key: &str) -> Result<Option<Value>, SourceError> {
        self.p
            .catalog()
            .cache_get_stale(self.account.id.as_str(), key)
            .map_err(host_err)
    }

    fn cache_put(&self, key: &str, value: &Value, ttl: Duration) -> Result<(), SourceError> {
        let ttl = chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::days(1));
        self.p
            .catalog()
            .cache_put(self.account.id.as_str(), key, value, ttl)
            .map_err(host_err)?;
        // The normalize snapshot may depend on the cache; reload it lazily.
        if let Ok(mut g) = self.snapshot.lock() {
            *g = None;
        }
        Ok(())
    }

    fn is_cancelled(&self) -> bool {
        self.p.is_cancelled()
    }

    fn now(&self) -> DateTime<Utc> {
        self.p.clock().now()
    }
}
