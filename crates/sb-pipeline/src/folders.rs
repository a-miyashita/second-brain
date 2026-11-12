//! Folder settings and the read-only check behind `sb folder`
//! (docs/specs/folder-sync.md, ADR-0022).

use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use second_brain_kernel::folder::{
    FolderConfig, FolderOps, FolderReport, cursor_key, sync_folders,
};
use second_brain_kernel::source::{SourceError, SyncHost};
use second_brain_kernel::{
    AccountKind, AccountStatus, CursorUpdate, DiscoveryBatch, QueueItem, Severity, SourceKind,
    SyncOptions as SourceSyncOptions,
};
use second_brain_store::{Account, CommitBatch};
use serde::Serialize;
use serde_json::Value;

use crate::Pipeline;
use crate::error::PipelineError;

/// Options of `sb folder add`; unset values keep the defaults of a folder.
#[derive(Debug, Clone, Default)]
pub struct FolderAdd {
    pub label: Option<String>,
    pub recursive: Option<bool>,
    pub max_depth: Option<u32>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub initial_days: Option<i64>,
    pub max_files: Option<usize>,
}

/// A configured folder with its coverage.
#[derive(Debug, Clone, Serialize)]
pub struct FolderInfo {
    pub account: String,
    pub source_kind: SourceKind,
    #[serde(flatten)]
    pub folder: FolderConfig,
    pub covered_since: Option<String>,
    pub listed_at: Option<String>,
}

/// The result of a read-only check of one folder.
#[derive(Debug, Clone, Serialize)]
pub struct FolderCheck {
    pub account: String,
    #[serde(flatten)]
    pub report: FolderReport,
    /// Modification times from here on are in scope.
    pub window_from: Option<String>,
    /// Total size of the files that a sync would fetch, where the provider says.
    pub bytes: u64,
    /// The biggest of them, `(name, bytes)`.
    pub largest: Vec<(String, u64)>,
    /// Problems the listing met (the issues a sync would open).
    pub problems: Vec<String>,
}

fn rejected(e: SourceError) -> PipelineError {
    match e {
        SourceError::Rejected(m) => PipelineError::Invalid(m),
        other => other.into(),
    }
}

fn describe(a: &Account, f: FolderConfig, cat: &second_brain_store::Catalog) -> FolderInfo {
    let kind = match a.kind {
        AccountKind::Microsoft => SourceKind::MicrosoftFile,
        _ => SourceKind::GoogleDoc,
    };
    let cursor = cat
        .cursor(&a.id, kind, &cursor_key(&f))
        .ok()
        .flatten()
        .unwrap_or(Value::Null);
    let get = |k: &str| cursor.get(k).and_then(Value::as_str).map(str::to_string);
    FolderInfo {
        account: a.id.to_string(),
        source_kind: kind,
        covered_since: get("covered_since"),
        listed_at: get("listed_at"),
        folder: f,
    }
}

fn select<'a>(
    folders: &'a [FolderConfig],
    selector: &str,
) -> Result<&'a FolderConfig, PipelineError> {
    let hits: Vec<&FolderConfig> = folders
        .iter()
        .filter(|f| f.key == selector || f.label == selector)
        .collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(PipelineError::Invalid(format!(
            "no folder {selector:?} on this account (see `sb folder list`)"
        ))),
        _ => Err(PipelineError::Invalid(format!(
            "{selector:?} matches several folders; use the folder key"
        ))),
    }
}

/// A host that reads the catalog and writes nothing: the queue rows a sync would
/// make are captured instead.
struct CheckHost<'a> {
    p: &'a Pipeline,
    account: &'a Account,
    queued: Mutex<Vec<QueueItem>>,
    issues: Mutex<Vec<String>>,
}

impl SyncHost for CheckHost<'_> {
    fn cursor(&self, kind: SourceKind, key: &str) -> Result<Option<Value>, SourceError> {
        self.p
            .catalog()
            .cursor(&self.account.id, kind, key)
            .map_err(|e| SourceError::Host(e.to_string()))
    }
    fn cursors(&self, _k: SourceKind, _p: &str) -> Result<Vec<(String, Value)>, SourceError> {
        Ok(Vec::new())
    }
    fn fetch_state(&self, kind: SourceKind, id: &str) -> Result<Option<Value>, SourceError> {
        Ok(self
            .p
            .catalog()
            .entry_by_key(self.account.id.as_str(), kind, id)
            .map_err(|e| SourceError::Host(e.to_string()))?
            .and_then(|e| e.fetch_state))
    }
    fn entry_exists(&self, kind: SourceKind, id: &str) -> Result<bool, SourceError> {
        Ok(self
            .p
            .catalog()
            .entry_by_key(self.account.id.as_str(), kind, id)
            .map_err(|e| SourceError::Host(e.to_string()))?
            .is_some())
    }
    fn commit(&self, batch: DiscoveryBatch) -> Result<(), SourceError> {
        if let Ok(mut q) = self.queued.lock() {
            q.extend(batch.enqueue);
        }
        Ok(())
    }
    fn cache_get(&self, _key: &str) -> Result<Option<Value>, SourceError> {
        Ok(None)
    }
    fn cache_put(&self, _k: &str, _v: &Value, _ttl: Duration) -> Result<(), SourceError> {
        Ok(())
    }
    fn is_cancelled(&self) -> bool {
        self.p.is_cancelled()
    }
    fn now(&self) -> DateTime<Utc> {
        self.p.clock().now()
    }
    fn issue(&self, _code: &str, _severity: Severity, message: &str) {
        if let Ok(mut i) = self.issues.lock() {
            i.push(message.to_string());
        }
    }
}

impl Pipeline {
    fn folder_account(&self, id: &str) -> Result<Account, PipelineError> {
        let a = self.account(id)?;
        if !matches!(a.kind, AccountKind::Google | AccountKind::Microsoft) {
            return Err(PipelineError::Invalid(format!(
                "{id} is not a Google or Microsoft account; folders need one"
            )));
        }
        Ok(a)
    }

    /// Add (or update) a folder of an account, resolved from its URL. Nothing is
    /// fetched or entered.
    pub async fn folder_add(
        &self,
        account: &str,
        url: &str,
        o: &FolderAdd,
    ) -> Result<FolderInfo, PipelineError> {
        let a = self.folder_account(account)?;
        if a.status == AccountStatus::Disabled {
            return Err(PipelineError::Invalid(format!("{account} is disabled")));
        }
        let source = self
            .source_for(&a)?
            .ok_or_else(|| PipelineError::Invalid(format!("{account} has no source")))?;
        let ops = source.folders().ok_or_else(|| {
            PipelineError::Invalid(format!("{account} cannot list folders (no credentials?)"))
        })?;
        let resolved = ops.resolve_folder(url).await.map_err(rejected)?;
        // The URL itself may carry a sharing token: keep it as given, in the
        // account config (0600), and never log it.
        let mut list = FolderConfig::list(&a.config);
        let mut f = list
            .iter()
            .find(|f| f.key == resolved.key)
            .cloned()
            .unwrap_or_default();
        f.key = resolved.key;
        f.url = url.trim().to_string();
        f.label = o
            .label
            .clone()
            .filter(|l| !l.trim().is_empty())
            .or_else(|| (!f.label.is_empty()).then(|| f.label.clone()))
            .unwrap_or(resolved.name);
        if let Some(v) = o.recursive {
            f.recursive = v;
        }
        if let Some(v) = o.max_depth {
            f.max_depth = v;
        }
        if !o.include.is_empty() {
            f.include = o.include.clone();
        }
        if !o.exclude.is_empty() {
            f.exclude = o.exclude.clone();
        }
        if let Some(v) = o.initial_days {
            f.initial_days = Some(v);
        }
        if let Some(v) = o.max_files {
            f.max_files = v;
        }
        if list.iter().any(|x| x.label == f.label && x.key != f.key) {
            return Err(PipelineError::Invalid(format!(
                "another folder of {account} is already labeled {:?}; use --label",
                f.label
            )));
        }
        match list.iter_mut().find(|x| x.key == f.key) {
            Some(slot) => *slot = f.clone(),
            None => list.push(f.clone()),
        }
        let mut config = a.config.clone();
        config["folders"] =
            serde_json::to_value(&list).map_err(|e| PipelineError::Invalid(e.to_string()))?;
        self.catalog()
            .update_account(&a.id, None, None, Some(&config))?;
        let cat = self.catalog();
        Ok(describe(&a, f, &cat))
    }

    /// The configured folders, optionally of one account.
    pub fn folder_list(&self, account: Option<&str>) -> Result<Vec<FolderInfo>, PipelineError> {
        let cat = self.catalog();
        let mut out = Vec::new();
        for a in cat.accounts()? {
            if !matches!(a.kind, AccountKind::Google | AccountKind::Microsoft) {
                continue;
            }
            if account.is_some_and(|w| w != a.id.as_str()) {
                continue;
            }
            for f in FolderConfig::list(&a.config) {
                out.push(describe(&a, f, &cat));
            }
        }
        Ok(out)
    }

    /// Remove a folder setting and its coverage. Entries stay.
    pub fn folder_remove(
        &self,
        account: &str,
        selector: &str,
    ) -> Result<FolderConfig, PipelineError> {
        let a = self.folder_account(account)?;
        let list = FolderConfig::list(&a.config);
        let gone = select(&list, selector)?.clone();
        let rest: Vec<&FolderConfig> = list.iter().filter(|f| f.key != gone.key).collect();
        let mut config = a.config.clone();
        config["folders"] =
            serde_json::to_value(&rest).map_err(|e| PipelineError::Invalid(e.to_string()))?;
        let kind = describe(&a, gone.clone(), &self.catalog()).source_kind;
        let cat = self.catalog();
        cat.update_account(&a.id, None, None, Some(&config))?;
        cat.commit_batch(&CommitBatch {
            account_id: Some(a.id.clone()),
            cursors: vec![CursorUpdate {
                source_kind: kind,
                key: cursor_key(&gone),
                value: None,
            }],
            ..Default::default()
        })?;
        Ok(gone)
    }

    /// Whether each configured folder's root can be read now, for `sb doctor
    /// --online`: `(account, label, Ok | reason)`. Accounts that need
    /// re-authentication or are disabled are left out (doctor reports those).
    pub async fn folders_online(
        &self,
    ) -> Result<Vec<(String, String, Result<(), String>)>, PipelineError> {
        let mut out = Vec::new();
        let accounts = self.catalog().accounts()?;
        for a in accounts {
            if !matches!(a.kind, AccountKind::Google | AccountKind::Microsoft)
                || a.status != AccountStatus::Active
            {
                continue;
            }
            let folders = FolderConfig::list(&a.config);
            if folders.is_empty() {
                continue;
            }
            let Some(source) = self.source_for(&a)? else {
                continue;
            };
            let Some(ops) = source.folders() else {
                continue;
            };
            for f in folders {
                let label = if f.label.is_empty() {
                    f.key.clone()
                } else {
                    f.label.clone()
                };
                let res = match ops.root_name(&f).await {
                    Ok(Some(_)) => Ok(()),
                    Ok(None) => Err("cannot be read (deleted, or access was lost)".to_string()),
                    Err(e) => Err(e.to_string()),
                };
                out.push((a.id.to_string(), label, res));
            }
        }
        Ok(out)
    }

    /// List the folders like a sync would and report what it would enter,
    /// without writing anything. `account` and `selector` narrow the folders.
    pub async fn folder_check(
        &self,
        account: Option<&str>,
        selector: Option<&str>,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<FolderCheck>, PipelineError> {
        let accounts: Vec<Account> = match account {
            Some(id) => vec![self.folder_account(id)?],
            None => self
                .catalog()
                .accounts()?
                .into_iter()
                .filter(|a| matches!(a.kind, AccountKind::Google | AccountKind::Microsoft))
                .collect(),
        };
        let mut out = Vec::new();
        for a in accounts {
            let all = FolderConfig::list(&a.config);
            let chosen: Vec<FolderConfig> = match selector {
                Some(s) => vec![select(&all, s)?.clone()],
                None => all,
            };
            if chosen.is_empty() || a.status == AccountStatus::Disabled {
                continue;
            }
            let source = self
                .source_for(&a)?
                .ok_or_else(|| PipelineError::Invalid(format!("{} has no source", a.id)))?;
            let ops: &dyn FolderOps = source
                .folders()
                .ok_or_else(|| PipelineError::Invalid(format!("{} cannot list folders", a.id)))?;
            let (global_days, account_days) = {
                let cat = self.catalog();
                (
                    cat.setting_or("sync.initial_days", 30i64)?,
                    a.config.get("initial_days").and_then(Value::as_i64),
                )
            };
            let now = self.clock().now();
            let opts = SourceSyncOptions {
                run_started_at: Some(now),
                initial_days: Some(account_days.filter(|d| *d > 0).unwrap_or(global_days)),
                since,
                ..Default::default()
            };
            for cfg in chosen {
                let host = CheckHost {
                    p: self,
                    account: &a,
                    queued: Mutex::new(Vec::new()),
                    issues: Mutex::new(Vec::new()),
                };
                let reports = sync_folders(&host, &opts, std::slice::from_ref(&cfg), ops)
                    .await
                    .map_err(rejected)?;
                let queued = host.queued.lock().map(|q| q.clone()).unwrap_or_default();
                let mut sized: Vec<(String, u64)> = queued
                    .iter()
                    .filter_map(|q| {
                        Some((
                            q.hint.get("name")?.as_str()?.to_string(),
                            q.hint.get("size")?.as_u64()?,
                        ))
                    })
                    .collect();
                let bytes = sized.iter().map(|s| s.1).sum();
                sized.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
                sized.truncate(5);
                let cursor = self
                    .catalog()
                    .cursor(&a.id, ops.kind(), &cursor_key(&cfg))?
                    .and_then(|c| {
                        c.get("covered_since")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    });
                let window_from = match since {
                    Some(s) => Some(second_brain_kernel::util::ts(s)),
                    None => cursor.or_else(|| {
                        Some(second_brain_kernel::util::ts(
                            now - chrono::Duration::days(
                                cfg.initial_days
                                    .filter(|d| *d > 0)
                                    .unwrap_or(opts.initial_days.unwrap_or(30)),
                            ),
                        ))
                    }),
                };
                out.push(FolderCheck {
                    account: a.id.to_string(),
                    report: reports.into_iter().next().unwrap_or_default(),
                    window_from,
                    bytes,
                    largest: sized,
                    problems: host.issues.lock().map(|i| i.clone()).unwrap_or_default(),
                });
            }
        }
        Ok(out)
    }
}
