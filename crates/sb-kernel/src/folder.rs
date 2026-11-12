//! Folder sync: the part shared by Google Drive and SharePoint/OneDrive
//! (docs/specs/folder-sync.md, ADR-0022).
//!
//! A provider supplies a [`FolderLister`] (the children of one folder). This
//! module walks the tree, applies the filters and the window, compares with the
//! catalog and queues the files that are new or changed, together with the
//! folder's coverage, in batches (ADR-0012).

use std::collections::HashSet;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::kinds::{Severity, SourceKind};
use crate::model::{CursorUpdate, DiscoveryBatch, QueueItem, SyncOptions};
use crate::source::{SourceError, SyncHost};
use crate::util::{glob_match_path, ts};

pub const DEFAULT_MAX_DEPTH: u32 = 8;
pub const DEFAULT_MAX_FILES: usize = 5000;
/// Queue rows committed together.
const BATCH: usize = 500;
/// Longest `folder_path` kept.
const MAX_PATH_CHARS: usize = 300;
/// Files that are never entered, whatever `include` says.
const BUILTIN_EXCLUDES: &[&str] = &["~$*", ".~lock.*", "Thumbs.db", ".DS_Store", "desktop.ini"];

/// A folder of an account's `config.folders`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderConfig {
    /// Google: the folder ID. Microsoft: `<driveId>/<itemId>`.
    pub key: String,
    pub url: String,
    pub label: String,
    pub recursive: bool,
    pub max_depth: u32,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub initial_days: Option<i64>,
    pub max_files: usize,
}

impl Default for FolderConfig {
    fn default() -> Self {
        FolderConfig {
            key: String::new(),
            url: String::new(),
            label: String::new(),
            recursive: true,
            max_depth: DEFAULT_MAX_DEPTH,
            include: Vec::new(),
            exclude: Vec::new(),
            initial_days: None,
            max_files: DEFAULT_MAX_FILES,
        }
    }
}

impl FolderConfig {
    /// The folders of an account config. Entries without a key are ignored.
    pub fn list(config: &Value) -> Vec<FolderConfig> {
        config
            .get("folders")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| serde_json::from_value::<FolderConfig>(v.clone()).ok())
            .filter(|f| !f.key.is_empty())
            .collect()
    }

    /// Whether a file passes `include` and `exclude`. `rel_path` is the path
    /// from the folder root with `/` separators, the file name last.
    pub fn allows(&self, rel_path: &str) -> bool {
        let hit = |pats: &[String]| pats.iter().any(|p| glob_match_path(p, rel_path));
        if BUILTIN_EXCLUDES
            .iter()
            .any(|p| glob_match_path(p, rel_path))
            || hit(&self.exclude)
        {
            return false;
        }
        self.include.is_empty() || hit(&self.include)
    }
}

/// A file as the provider lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderFile {
    /// The natural key of the entry (`source_id`).
    pub id: String,
    pub name: String,
    pub modified: Option<DateTime<Utc>>,
    /// The modification time as the provider wrote it.
    pub modified_raw: Option<String>,
    pub etag: Option<String>,
    pub size: Option<u64>,
}

/// One child of a folder.
#[derive(Debug, Clone, PartialEq)]
pub enum Child {
    /// A subfolder; `id` is what [`FolderLister::children`] takes.
    Folder {
        id: String,
        name: String,
    },
    File(FolderFile),
    /// Something that cannot become an entry (form, image, OneNote, a shortcut to
    /// a folder, ...).
    Unsupported,
}

/// The provider side of a folder sync.
#[async_trait]
pub trait FolderLister: Send + Sync {
    /// The name of the configured root folder, or `None` when it is gone or no
    /// longer readable.
    async fn root_name(&self, folder: &FolderConfig) -> Result<Option<String>, SourceError>;
    /// All children of a folder (every page).
    async fn children(&self, folder_id: &str) -> Result<Vec<Child>, SourceError>;
}

/// A listed file with its place in the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanFile {
    pub file: FolderFile,
    /// The folder path from the root's own name: `Minutes / 2026 / Q3`.
    pub folder_path: String,
}

/// What a walk found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scan {
    pub files: Vec<ScanFile>,
    pub unsupported: u64,
    pub excluded: u64,
    /// Subfolders deeper than `max_depth`.
    pub too_deep: u64,
}

/// `Minutes / 2026 / Q3`, shortened to [`MAX_PATH_CHARS`] by keeping the first
/// segment and as many of the last ones as fit, with `...` between.
pub fn join_path(segments: &[String]) -> String {
    let full = segments.join(" / ");
    if full.chars().count() <= MAX_PATH_CHARS || segments.len() < 3 {
        return full.chars().take(MAX_PATH_CHARS).collect();
    }
    let head = &segments[0];
    let mut tail: Vec<&String> = Vec::new();
    let mut len = head.chars().count() + " / ... ".chars().count();
    for s in segments[1..].iter().rev() {
        let add = s.chars().count() + 3;
        if len + add > MAX_PATH_CHARS {
            break;
        }
        len += add;
        tail.push(s);
    }
    tail.reverse();
    if tail.is_empty() {
        return head.chars().take(MAX_PATH_CHARS).collect();
    }
    format!(
        "{head} / ... / {}",
        tail.iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(" / ")
    )
}

/// Walk the folder breadth first.
pub async fn scan(
    lister: &dyn FolderLister,
    cfg: &FolderConfig,
    root_name: &str,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> Result<Scan, SourceError> {
    let mut out = Scan::default();
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(cfg.key.clone());
    // (folder id, names from the root's own name, depth)
    let mut queue: std::collections::VecDeque<(String, Vec<String>, u32)> =
        std::collections::VecDeque::new();
    queue.push_back((cfg.key.clone(), vec![root_name.to_string()], 0));
    while let Some((id, names, depth)) = queue.pop_front() {
        if cancelled() {
            return Err(SourceError::Cancelled);
        }
        for child in lister.children(&id).await? {
            match child {
                Child::Unsupported => out.unsupported += 1,
                Child::Folder { id: sub, name } => {
                    if !cfg.recursive {
                        continue;
                    }
                    if seen.contains(&sub) {
                        continue;
                    }
                    if depth >= cfg.max_depth {
                        out.too_deep += 1;
                        continue;
                    }
                    seen.insert(sub.clone());
                    let mut n = names.clone();
                    n.push(name);
                    queue.push_back((sub, n, depth + 1));
                }
                Child::File(file) => {
                    let rel: Vec<&str> = names[1..]
                        .iter()
                        .map(String::as_str)
                        .chain(std::iter::once(file.name.as_str()))
                        .collect();
                    if !cfg.allows(&rel.join("/")) {
                        out.excluded += 1;
                        continue;
                    }
                    out.files.push(ScanFile {
                        file,
                        folder_path: join_path(&names),
                    });
                }
            }
        }
    }
    Ok(out)
}

/// The files in scope and the coverage to record.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// Inclusive lower bound of the modification time.
    pub from: DateTime<Utc>,
    /// Exclusive upper bound (`--until`).
    pub until: Option<DateTime<Utc>>,
    /// The `covered_since` to store afterwards.
    pub covered_since: DateTime<Utc>,
}

/// The window of a run (folder-sync.md "Coverage"). `stored` is the folder's
/// `covered_since`, if it has one.
pub fn plan_window(
    stored: Option<DateTime<Utc>>,
    run_start: DateTime<Utc>,
    initial_days: i64,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> Window {
    let base = stored.unwrap_or(run_start - Duration::days(initial_days));
    match (since, until) {
        (Some(x), None) if x < base => Window {
            from: x,
            until: None,
            covered_since: x,
        },
        (Some(x), Some(y)) => Window {
            from: x,
            until: Some(y),
            covered_since: if y >= base { base.min(x) } else { base },
        },
        _ => Window {
            from: base,
            until: None,
            covered_since: base,
        },
    }
}

impl Window {
    pub fn contains(&self, modified: Option<DateTime<Utc>>) -> bool {
        match modified {
            // A file without a time cannot be placed: it is in scope.
            None => true,
            Some(m) => m >= self.from && self.until.is_none_or(|u| m < u),
        }
    }
}

/// How the provider compares a listed file with the state stored in the entry.
pub type ChangedFn = dyn Fn(&FolderFile, &Value) -> bool + Send + Sync;

/// Provider parameters of [`sync_folders`].
pub struct FolderSyncParams<'a> {
    /// `google.doc` or `microsoft.file`.
    pub kind: SourceKind,
    pub lister: &'a dyn FolderLister,
    /// Whether a listed file differs from the stored `fetch_state`.
    pub changed: &'a ChangedFn,
    /// Kinds in which an existing entry with the same ID means "already known"
    /// (a Gemini notes document that is a `google.meet` entry).
    pub duplicate_kinds: &'a [SourceKind],
}

/// What happened to one folder in one run.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct FolderReport {
    pub folder: String,
    pub label: String,
    /// Files in the listing that passed the filters.
    pub listed: u64,
    pub in_scope: u64,
    pub new: u64,
    pub changed: u64,
    pub unchanged: u64,
    pub duplicate: u64,
    pub unsupported: u64,
    pub excluded: u64,
    pub too_deep: u64,
    /// `ok`, `too_large`, `unavailable` or `failed`.
    pub status: String,
}

/// Cursor key of a folder.
pub fn cursor_key(cfg: &FolderConfig) -> String {
    format!("folder:{}", cfg.key)
}

fn hint(cfg: &FolderConfig, f: &ScanFile) -> Value {
    // Shaped like `IngestHint`, so that the source's fetch picks `extra` up.
    json!({
        "extra": {"sync_folder": cfg.key, "folder_path": f.folder_path},
        "modified_time": f.file.modified_raw,
        "etag": f.file.etag,
    })
}

/// Sync the configured folders: list, compare, queue, record coverage. Problems
/// of one folder are reported as issues and do not stop the others.
pub async fn sync_folders(
    host: &dyn SyncHost,
    opts: &SyncOptions,
    configs: &[FolderConfig],
    p: &FolderSyncParams<'_>,
) -> Result<Vec<FolderReport>, SourceError> {
    let mut reports = Vec::new();
    for cfg in configs {
        if host.is_cancelled() {
            return Err(SourceError::Cancelled);
        }
        let label = if cfg.label.is_empty() {
            cfg.key.clone()
        } else {
            cfg.label.clone()
        };
        let mut report = FolderReport {
            folder: cfg.key.clone(),
            label: label.clone(),
            status: "ok".into(),
            ..Default::default()
        };
        match sync_one(host, opts, cfg, p, &mut report).await {
            Ok(()) => {}
            Err(SourceError::Cancelled) => return Err(SourceError::Cancelled),
            Err(e @ SourceError::Auth(_)) => return Err(e),
            Err(e) => {
                report.status = "failed".into();
                host.issue(
                    "folder.failed",
                    Severity::Warning,
                    &format!("folder {label}: {e}"),
                );
            }
        }
        reports.push(report);
    }
    Ok(reports)
}

async fn sync_one(
    host: &dyn SyncHost,
    opts: &SyncOptions,
    cfg: &FolderConfig,
    p: &FolderSyncParams<'_>,
    report: &mut FolderReport,
) -> Result<(), SourceError> {
    let label = &report.label.clone();
    let Some(root_name) = p.lister.root_name(cfg).await? else {
        report.status = "unavailable".into();
        host.issue(
            "folder.unavailable",
            Severity::Error,
            &format!("folder {label} cannot be read (deleted, or access was lost)"),
        );
        return Ok(());
    };
    let run_start = opts.run_start(host.now());
    let key = cursor_key(cfg);
    let stored = host
        .cursor(p.kind, &key)?
        .and_then(|v| {
            v.get("covered_since")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .and_then(|s| crate::util::parse_ts(&s));
    let initial_days = cfg
        .initial_days
        .filter(|d| *d > 0)
        .or(opts.initial_days)
        .unwrap_or(crate::coverage::DEFAULT_INITIAL_DAYS);
    let window = plan_window(stored, run_start, initial_days, opts.since, opts.until);

    let scan = scan(p.lister, cfg, &root_name, &|| host.is_cancelled()).await?;
    report.listed = scan.files.len() as u64;
    report.unsupported = scan.unsupported;
    report.excluded = scan.excluded;
    report.too_deep = scan.too_deep;

    let in_scope: Vec<&ScanFile> = scan
        .files
        .iter()
        .filter(|f| window.contains(f.file.modified))
        .collect();
    report.in_scope = in_scope.len() as u64;
    if in_scope.len() > cfg.max_files {
        report.status = "too_large".into();
        host.issue(
            "folder.too_large",
            Severity::Warning,
            &format!(
                "folder {label}: {} files are in scope, more than max_files ({}); \
                 raise max_files, narrow include/exclude, or reduce initial_days",
                in_scope.len(),
                cfg.max_files
            ),
        );
        return Ok(());
    }

    let mut enqueue: Vec<QueueItem> = Vec::new();
    for f in in_scope {
        let id = &f.file.id;
        let reason = if host.entry_exists(p.kind, id)? {
            let changed = match host.fetch_state(p.kind, id)? {
                Some(state) => (p.changed)(&f.file, &state),
                None => true,
            };
            if !changed {
                report.unchanged += 1;
                continue;
            }
            report.changed += 1;
            "folder_changed"
        } else {
            let mut known = false;
            for k in p.duplicate_kinds {
                if host.entry_exists(*k, id)? {
                    known = true;
                    break;
                }
            }
            if known {
                report.duplicate += 1;
                continue;
            }
            report.new += 1;
            "folder_new"
        };
        enqueue.push(QueueItem {
            source_kind: p.kind,
            source_id: id.clone(),
            reason: reason.into(),
            hint: hint(cfg, f),
        });
    }

    let cursor = CursorUpdate {
        source_kind: p.kind,
        key,
        value: Some(json!({
            "covered_since": ts(window.covered_since),
            "listed_at": ts(host.now()),
        })),
    };
    // The coverage goes with the last batch, so that it never runs ahead of the
    // queue rows it covers (ADR-0012).
    let mut chunks: Vec<Vec<QueueItem>> = enqueue.chunks(BATCH).map(<[_]>::to_vec).collect();
    if chunks.is_empty() {
        chunks.push(Vec::new());
    }
    let last = chunks.len() - 1;
    for (i, chunk) in chunks.into_iter().enumerate() {
        host.commit(DiscoveryBatch {
            entries: Vec::new(),
            enqueue: chunk,
            cursors: if i == last {
                vec![cursor.clone()]
            } else {
                Vec::new()
            },
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::parse_ts;
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Mutex;

    fn t(s: &str) -> DateTime<Utc> {
        parse_ts(s).unwrap()
    }

    fn file(id: &str, name: &str, modified: &str) -> Child {
        Child::File(FolderFile {
            id: id.into(),
            name: name.into(),
            modified: parse_ts(modified),
            modified_raw: Some(modified.into()),
            etag: None,
            size: None,
        })
    }

    fn folder(id: &str, name: &str) -> Child {
        Child::Folder {
            id: id.into(),
            name: name.into(),
        }
    }

    /// Children by folder ID.
    struct Tree(HashMap<String, Vec<Child>>, Option<String>);

    #[async_trait]
    impl FolderLister for Tree {
        async fn root_name(&self, _f: &FolderConfig) -> Result<Option<String>, SourceError> {
            Ok(self.1.clone())
        }
        async fn children(&self, id: &str) -> Result<Vec<Child>, SourceError> {
            Ok(self.0.get(id).cloned().unwrap_or_default())
        }
    }

    fn tree() -> Tree {
        let mut m = HashMap::new();
        m.insert(
            "root".to_string(),
            vec![
                file("f1", "plan.docx", "2026-09-05T00:00:00Z"),
                file("f2", "~$plan.docx", "2026-09-05T00:00:00Z"),
                file("old", "old.docx", "2025-01-01T00:00:00Z"),
                folder("sub", "2026"),
                folder("loop", "again"),
                Child::Unsupported,
            ],
        );
        m.insert(
            "sub".to_string(),
            vec![
                file("f3", "minutes.pdf", "2026-09-08T00:00:00Z"),
                folder("deep", "Q3"),
                folder("root", "back to root"),
            ],
        );
        m.insert(
            "deep".to_string(),
            vec![file("f4", "notes.md", "2026-09-09T00:00:00Z")],
        );
        m.insert("loop".to_string(), vec![folder("loop", "itself")]);
        Tree(m, Some("Minutes".into()))
    }

    fn cfg() -> FolderConfig {
        FolderConfig {
            key: "root".into(),
            label: "Minutes".into(),
            ..Default::default()
        }
    }

    #[derive(Default)]
    struct FakeHost {
        entries: Mutex<BTreeMap<(SourceKind, String), Value>>,
        cursors: Mutex<BTreeMap<String, Value>>,
        queue: Mutex<Vec<QueueItem>>,
        batches: Mutex<u32>,
        issues: Mutex<Vec<(String, String)>>,
    }

    impl SyncHost for FakeHost {
        fn cursor(&self, _k: SourceKind, key: &str) -> Result<Option<Value>, SourceError> {
            Ok(self.cursors.lock().unwrap().get(key).cloned())
        }
        fn cursors(&self, _k: SourceKind, _p: &str) -> Result<Vec<(String, Value)>, SourceError> {
            Ok(vec![])
        }
        fn fetch_state(&self, k: SourceKind, id: &str) -> Result<Option<Value>, SourceError> {
            Ok(self
                .entries
                .lock()
                .unwrap()
                .get(&(k, id.to_string()))
                .filter(|v| !v.is_null())
                .cloned())
        }
        fn entry_exists(&self, k: SourceKind, id: &str) -> Result<bool, SourceError> {
            Ok(self
                .entries
                .lock()
                .unwrap()
                .contains_key(&(k, id.to_string())))
        }
        fn commit(&self, b: DiscoveryBatch) -> Result<(), SourceError> {
            *self.batches.lock().unwrap() += 1;
            self.queue.lock().unwrap().extend(b.enqueue);
            for c in b.cursors {
                self.cursors
                    .lock()
                    .unwrap()
                    .insert(c.key, c.value.unwrap_or(Value::Null));
            }
            Ok(())
        }
        fn cache_get(&self, _k: &str) -> Result<Option<Value>, SourceError> {
            Ok(None)
        }
        fn cache_put(
            &self,
            _k: &str,
            _v: &Value,
            _t: std::time::Duration,
        ) -> Result<(), SourceError> {
            Ok(())
        }
        fn is_cancelled(&self) -> bool {
            false
        }
        fn now(&self) -> DateTime<Utc> {
            t("2026-09-10T00:00:00Z")
        }
        fn issue(&self, code: &str, _s: Severity, message: &str) {
            self.issues
                .lock()
                .unwrap()
                .push((code.into(), message.into()));
        }
    }

    fn changed() -> impl Fn(&FolderFile, &Value) -> bool + Send + Sync {
        |f, state| state.get("modified_time").and_then(Value::as_str) != f.modified_raw.as_deref()
    }

    fn opts() -> SyncOptions {
        SyncOptions {
            run_started_at: Some(t("2026-09-10T00:00:00Z")),
            initial_days: Some(30),
            ..Default::default()
        }
    }

    async fn run(host: &FakeHost, tree: &Tree, o: &SyncOptions, c: &FolderConfig) -> FolderReport {
        let ch = changed();
        let p = FolderSyncParams {
            kind: SourceKind::GoogleDoc,
            lister: tree,
            changed: &ch,
            duplicate_kinds: &[SourceKind::GoogleMeet],
        };
        sync_folders(host, o, std::slice::from_ref(c), &p)
            .await
            .unwrap()
            .remove(0)
    }

    fn queued(h: &FakeHost) -> Vec<(String, String)> {
        let mut q: Vec<_> = h
            .queue
            .lock()
            .unwrap()
            .iter()
            .map(|i| (i.source_id.clone(), i.reason.clone()))
            .collect();
        q.sort();
        q
    }

    #[tokio::test]
    async fn walks_the_tree_filters_and_queues_files_in_the_window() {
        let h = FakeHost::default();
        let r = run(&h, &tree(), &opts(), &cfg()).await;
        assert_eq!(r.status, "ok");
        // f2 is a lock file, `old` is before the 30-day window; the loop and the
        // back-reference to the root are visited once.
        assert_eq!(
            queued(&h),
            vec![
                ("f1".into(), "folder_new".into()),
                ("f3".into(), "folder_new".into()),
                ("f4".into(), "folder_new".into()),
            ]
        );
        assert_eq!((r.listed, r.in_scope, r.new), (4, 3, 3));
        assert_eq!((r.unsupported, r.excluded), (1, 1));
        let item = h
            .queue
            .lock()
            .unwrap()
            .iter()
            .find(|i| i.source_id == "f4")
            .cloned()
            .unwrap();
        assert_eq!(item.hint["extra"]["folder_path"], "Minutes / 2026 / Q3");
        assert_eq!(item.hint["extra"]["sync_folder"], "root");
        assert_eq!(item.hint["modified_time"], "2026-09-09T00:00:00Z");
        // Coverage is recorded: the first window is 30 days.
        let c = h
            .cursors
            .lock()
            .unwrap()
            .get("folder:root")
            .cloned()
            .unwrap();
        assert_eq!(c["covered_since"], "2026-08-11T00:00:00Z");
    }

    #[tokio::test]
    async fn known_files_are_compared_and_meet_duplicates_skipped() {
        let h = FakeHost::default();
        {
            let mut e = h.entries.lock().unwrap();
            // f1 unchanged; f3 changed; f4 is a Gemini notes document.
            e.insert(
                (SourceKind::GoogleDoc, "f1".into()),
                json!({"modified_time": "2026-09-05T00:00:00Z"}),
            );
            e.insert(
                (SourceKind::GoogleDoc, "f3".into()),
                json!({"modified_time": "2026-09-01T00:00:00Z"}),
            );
            e.insert((SourceKind::GoogleMeet, "f4".into()), Value::Null);
        }
        let r = run(&h, &tree(), &opts(), &cfg()).await;
        assert_eq!(queued(&h), vec![("f3".into(), "folder_changed".into())]);
        assert_eq!((r.new, r.changed, r.unchanged, r.duplicate), (0, 1, 1, 1));
    }

    #[tokio::test]
    async fn an_existing_entry_without_state_is_fetched_again() {
        let h = FakeHost::default();
        h.entries
            .lock()
            .unwrap()
            .insert((SourceKind::GoogleDoc, "f1".into()), Value::Null);
        run(&h, &tree(), &opts(), &cfg()).await;
        assert!(queued(&h).contains(&("f1".into(), "folder_changed".into())));
    }

    #[tokio::test]
    async fn include_exclude_depth_and_non_recursive() {
        let h = FakeHost::default();
        let mut c = cfg();
        c.include = vec!["*.pdf".into(), "2026/Q3/**".into()];
        run(&h, &tree(), &opts(), &c).await;
        assert_eq!(
            queued(&h).into_iter().map(|x| x.0).collect::<Vec<_>>(),
            vec!["f3", "f4"]
        );

        let h = FakeHost::default();
        let mut c = cfg();
        c.max_depth = 1;
        let r = run(&h, &tree(), &opts(), &c).await;
        assert_eq!(r.too_deep, 1, "Q3 is below depth 1");
        assert!(!queued(&h).iter().any(|x| x.0 == "f4"));

        let h = FakeHost::default();
        let mut c = cfg();
        c.recursive = false;
        run(&h, &tree(), &opts(), &c).await;
        assert_eq!(queued(&h), vec![("f1".into(), "folder_new".into())]);

        let h = FakeHost::default();
        let mut c = cfg();
        c.exclude = vec!["2026/**".into()];
        run(&h, &tree(), &opts(), &c).await;
        assert_eq!(queued(&h), vec![("f1".into(), "folder_new".into())]);
    }

    #[tokio::test]
    async fn since_extends_the_window_backwards_and_later_runs_keep_it() {
        let h = FakeHost::default();
        run(&h, &tree(), &opts(), &cfg()).await;
        assert!(!queued(&h).iter().any(|x| x.0 == "old"));
        // --since 2024-12-01 brings in the old file and moves coverage back.
        let mut o = opts();
        o.since = Some(t("2024-12-01T00:00:00Z"));
        h.queue.lock().unwrap().clear();
        let r = run(&h, &tree(), &o, &cfg()).await;
        assert!(queued(&h).iter().any(|x| x.0 == "old"), "{r:?}");
        let c = h
            .cursors
            .lock()
            .unwrap()
            .get("folder:root")
            .cloned()
            .unwrap();
        assert_eq!(c["covered_since"], "2024-12-01T00:00:00Z");
        // A later plain run keeps that coverage.
        h.queue.lock().unwrap().clear();
        run(&h, &tree(), &opts(), &cfg()).await;
        assert!(queued(&h).iter().any(|x| x.0 == "old"));
        // A --since inside the coverage changes nothing.
        let mut o = opts();
        o.since = Some(t("2026-01-01T00:00:00Z"));
        run(&h, &tree(), &o, &cfg()).await;
        let c = h
            .cursors
            .lock()
            .unwrap()
            .get("folder:root")
            .cloned()
            .unwrap();
        assert_eq!(c["covered_since"], "2024-12-01T00:00:00Z");
    }

    #[test]
    fn window_rules() {
        let run = t("2026-09-10T00:00:00Z");
        let w = plan_window(None, run, 30, None, None);
        assert_eq!(w.from, t("2026-08-11T00:00:00Z"));
        let stored = Some(t("2026-06-01T00:00:00Z"));
        // Explicit range: used as it is; coverage moves only if it reaches the covered part.
        let w = plan_window(
            stored,
            run,
            30,
            Some(t("2026-01-01T00:00:00Z")),
            Some(t("2026-02-01T00:00:00Z")),
        );
        assert_eq!(
            (w.from, w.until),
            (t("2026-01-01T00:00:00Z"), Some(t("2026-02-01T00:00:00Z")))
        );
        assert_eq!(w.covered_since, t("2026-06-01T00:00:00Z"));
        let w = plan_window(
            stored,
            run,
            30,
            Some(t("2026-01-01T00:00:00Z")),
            Some(t("2026-07-01T00:00:00Z")),
        );
        assert_eq!(w.covered_since, t("2026-01-01T00:00:00Z"));
        assert!(w.contains(None));
        assert!(!w.contains(Some(t("2026-07-01T00:00:00Z"))));
    }

    #[tokio::test]
    async fn too_many_files_stop_the_folder_without_coverage() {
        let h = FakeHost::default();
        let mut c = cfg();
        c.max_files = 2;
        let r = run(&h, &tree(), &opts(), &c).await;
        assert_eq!(r.status, "too_large");
        assert!(queued(&h).is_empty());
        assert!(h.cursors.lock().unwrap().is_empty());
        assert_eq!(h.issues.lock().unwrap()[0].0, "folder.too_large");
    }

    #[tokio::test]
    async fn an_unreadable_root_is_an_issue_and_leaves_everything_as_it_is() {
        let h = FakeHost::default();
        let mut tr = tree();
        tr.1 = None;
        let r = run(&h, &tr, &opts(), &cfg()).await;
        assert_eq!(r.status, "unavailable");
        assert_eq!(h.issues.lock().unwrap()[0].0, "folder.unavailable");
        assert!(h.cursors.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn queue_rows_go_in_batches_and_the_cursor_with_the_last() {
        let mut m = HashMap::new();
        m.insert(
            "root".to_string(),
            (0..1200)
                .map(|i| {
                    file(
                        &format!("f{i}"),
                        &format!("a{i}.docx"),
                        "2026-09-05T00:00:00Z",
                    )
                })
                .collect(),
        );
        let tr = Tree(m, Some("Big".into()));
        let h = FakeHost::default();
        let mut c = cfg();
        c.max_files = 5000;
        run(&h, &tr, &opts(), &c).await;
        assert_eq!(h.queue.lock().unwrap().len(), 1200);
        assert_eq!(*h.batches.lock().unwrap(), 3);
    }

    #[test]
    fn paths_are_shortened_in_the_middle() {
        let segs: Vec<String> = ["Root", "2026", "Q3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(join_path(&segs), "Root / 2026 / Q3");
        let long: Vec<String> = std::iter::once("Root".to_string())
            .chain((0..40).map(|i| format!("segment-number-{i:02}")))
            .collect();
        let p = join_path(&long);
        assert!(p.chars().count() <= 300, "{}", p.chars().count());
        assert!(p.starts_with("Root / ... / "));
        assert!(p.ends_with("segment-number-39"));
    }

    #[test]
    fn config_is_read_from_the_account_config() {
        let v = json!({"folders": [
            {"key": "a", "url": "u", "label": "L", "include": ["*.pdf"]},
            {"url": "no key"},
            "junk"
        ]});
        let f = FolderConfig::list(&v);
        assert_eq!(f.len(), 1);
        assert_eq!(
            (f[0].recursive, f[0].max_depth, f[0].max_files),
            (true, 8, 5000)
        );
        assert!(f[0].allows("x/a.pdf") && !f[0].allows("x/a.docx"));
        assert!(!FolderConfig::default().allows("~$a.docx"));
        assert!(FolderConfig::list(&json!({})).is_empty());
    }
}
