//! `sb import`: the `second-brain-import/v1` bundle format (import-format.md).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use second_brain_kernel::{
    AccountId, AccountKind, Generator, GeneratorKind, RawObject, RawRole, RawStatus, RunStatus,
    SectionDraft, SectionKind, SectionOrigin, SourceKind, SourceRef,
};
use second_brain_store::import::{ImportResult, ImportWrite};
use second_brain_store::rawstore;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Pipeline;
use crate::error::PipelineError;
use crate::run::Stop;

/// The only supported format.
pub const FORMAT: &str = "second-brain-import/v1";

#[derive(Debug, Deserialize)]
struct Manifest {
    format: String,
    #[serde(default)]
    accounts: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct RecordSection {
    kind: SectionKind,
    #[serde(default = "default_origin")]
    origin: SectionOrigin,
    text: String,
}

fn default_origin() -> SectionOrigin {
    SectionOrigin::Generated
}

#[derive(Debug, Deserialize)]
struct RecordSummary {
    generator_kind: GeneratorKind,
    provider: String,
    model: String,
    #[serde(default)]
    prompt_version: Option<String>,
    #[serde(default)]
    generated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct RecordRaw {
    role: RawRole,
    path: String,
    #[serde(default)]
    media_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Record {
    #[serde(default)]
    account: Option<String>,
    source_kind: SourceKind,
    source_id: String,
    #[serde(default)]
    source_url: Option<String>,
    title: String,
    #[serde(default)]
    source_created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    source_updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    ingested_at: Option<DateTime<Utc>>,
    #[serde(default)]
    metadata: Option<Value>,
    sections: Vec<RecordSection>,
    #[serde(default)]
    summary: Option<RecordSummary>,
    #[serde(default)]
    raw: Vec<RecordRaw>,
}

/// A problem with one line of `entries.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LineError {
    pub line: usize,
    pub message: String,
}

/// Result of an import.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ImportReport {
    pub run_id: Option<i64>,
    pub dry_run: bool,
    pub lines: u64,
    pub created: u64,
    pub updated: u64,
    pub unchanged: u64,
    /// Existing entries with raw data, where only missing fields were filled in.
    pub merged_into_present: u64,
    pub invalid: u64,
    pub raw_missing: u64,
    pub errors: Vec<LineError>,
    pub stop: Option<Stop>,
}

/// A validated record ready to write.
struct Prepared {
    source_ref: SourceRef,
    record: Record,
    raw_files: Vec<(RawRole, PathBuf, String)>,
}

fn safe_relative(p: &str) -> Option<PathBuf> {
    let path = Path::new(p);
    if path.is_absolute() || p.is_empty() {
        return None;
    }
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return None;
    }
    Some(path.to_path_buf())
}

fn media_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("md") => "text/markdown",
        Some("jsonl") => "application/x-ndjson",
        Some("json") => "application/json",
        Some("html") | Some("htm") => "text/html",
        Some("txt") => "text/plain",
        _ => "application/octet-stream",
    }
}

impl Pipeline {
    fn prepare_record(
        &self,
        bundle: &Path,
        mapping: &BTreeMap<String, String>,
        line: &str,
    ) -> Result<Prepared, String> {
        let record: Record =
            serde_json::from_str(line).map_err(|e| format!("invalid record: {e}"))?;
        if record.source_id.trim().is_empty() {
            return Err("source_id is empty".into());
        }
        if record.title.trim().is_empty() {
            return Err("title is empty".into());
        }
        if record.sections.is_empty() {
            return Err("at least one section is required".into());
        }
        let kind = record.source_kind.account_kind();
        let account = record
            .account
            .clone()
            .or_else(|| mapping.get(kind.as_str()).cloned())
            .ok_or_else(|| format!("no account for kind {kind} (add \"account\", a manifest mapping, or --map {kind}=<id>)"))?;
        let acct = self
            .catalog()
            .account_by_str(&account)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                format!("account {account:?} does not exist (run `sb account add` first)")
            })?;
        if acct.kind != kind && !(kind == AccountKind::Imap && acct.kind == AccountKind::Google) {
            return Err(format!(
                "account {account} is a {} account, but {} needs {kind}",
                acct.kind, record.source_kind
            ));
        }
        let mut raw_files = Vec::new();
        for r in &record.raw {
            let rel = safe_relative(&r.path).ok_or_else(|| {
                format!(
                    "raw path {:?} must be relative and inside the bundle",
                    r.path
                )
            })?;
            let full = bundle.join(&rel);
            if !full.is_file() {
                return Err(format!("raw file {:?} not found", r.path));
            }
            let mt = r
                .media_type
                .clone()
                .unwrap_or_else(|| media_type_for(&rel).to_string());
            raw_files.push((r.role, full, mt));
        }
        let account_id = AccountId::new(account).map_err(|e| e.to_string())?;
        Ok(Prepared {
            source_ref: SourceRef {
                account_id,
                source_kind: record.source_kind,
                source_id: record.source_id.clone(),
                source_url: record.source_url.clone(),
                created_at: record.source_created_at,
                updated_at: record.source_updated_at,
            },
            record,
            raw_files,
        })
    }

    fn write_prepared(&self, p: Prepared, report: &mut ImportReport) -> Result<(), PipelineError> {
        let rec = &p.record;
        let import_ref = rec
            .metadata
            .as_ref()
            .and_then(|m| m.get("import_ref"))
            .cloned();
        let existing = self.catalog().entry_by_key(
            p.source_ref.account_id.as_str(),
            rec.source_kind,
            &rec.source_id,
        )?;
        // Never downgrade an entry whose raw data is present.
        if let Some(e) = &existing
            && e.raw_status == RawStatus::Present
        {
            if self.catalog().merge_import_into_present(
                e.id,
                rec.ingested_at,
                import_ref.as_ref(),
            )? {
                report.merged_into_present += 1;
            } else {
                report.unchanged += 1;
            }
            return Ok(());
        }
        let now = self.clock().now();
        let mut stored = Vec::new();
        if !p.raw_files.is_empty() {
            let dir = rawstore::entry_dir(&p.source_ref, now);
            let mut seqs: BTreeMap<RawRole, i64> = BTreeMap::new();
            for (role, path, media_type) in &p.raw_files {
                let bytes = std::fs::read(path)
                    .map_err(|e| PipelineError::Invalid(format!("{}: {e}", path.display())))?;
                let seq = seqs.entry(*role).or_insert(0);
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("bin")
                    .to_string();
                stored.push(rawstore::write_object(
                    self.home(),
                    &dir,
                    *seq,
                    &RawObject {
                        role: *role,
                        media_type: media_type.clone(),
                        ext,
                        bytes,
                    },
                )?);
                *seq += 1;
            }
        }
        let base_meta = existing
            .as_ref()
            .map(|e| e.metadata.clone())
            .unwrap_or(Value::Object(Default::default()));
        let metadata =
            crate::host::merge_json(&base_meta, rec.metadata.as_ref().unwrap_or(&Value::Null));
        let write = ImportWrite {
            source_ref: p.source_ref.clone(),
            title: rec.title.clone(),
            ingested_at: rec.ingested_at,
            metadata,
            sections: rec
                .sections
                .iter()
                .map(|s| SectionDraft {
                    kind: s.kind,
                    origin: s.origin,
                    text: s.text.clone(),
                })
                .collect(),
            summary: rec.summary.as_ref().map(|s| {
                (
                    Generator {
                        kind: s.generator_kind,
                        provider: s.provider.clone(),
                        model: s.model.clone(),
                        prompt_version: s.prompt_version.clone(),
                    },
                    s.generated_at,
                )
            }),
            raw: stored,
        };
        let (res, _) = self.catalog().apply_import(&write)?;
        match res {
            ImportResult::Created => report.created += 1,
            ImportResult::Updated => report.updated += 1,
            ImportResult::Unchanged => report.unchanged += 1,
        }
        if write.raw.is_empty() {
            report.raw_missing += 1;
        }
        Ok(())
    }

    /// Import a bundle. `map` holds `kind=account` overrides.
    pub fn import_bundle(
        &self,
        bundle: &Path,
        map: &[(String, String)],
        dry_run: bool,
    ) -> Result<ImportReport, PipelineError> {
        let manifest_path = bundle.join("manifest.json");
        let manifest: Manifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).map_err(|e| {
                PipelineError::Invalid(format!("{}: {e}", manifest_path.display()))
            })?)
            .map_err(|e| PipelineError::Invalid(format!("manifest.json: {e}")))?;
        if manifest.format != FORMAT {
            return Err(PipelineError::Invalid(format!(
                "unsupported bundle format {:?} (expected {FORMAT})",
                manifest.format
            )));
        }
        let mut mapping = manifest.accounts.clone();
        for (k, v) in map {
            k.parse::<AccountKind>()
                .map_err(|e| PipelineError::Invalid(e.to_string()))?;
            mapping.insert(k.clone(), v.clone());
        }
        let entries_path = bundle.join("entries.jsonl");
        let file = std::fs::File::open(&entries_path)
            .map_err(|e| PipelineError::Invalid(format!("{}: {e}", entries_path.display())))?;
        let mut report = ImportReport {
            dry_run,
            ..Default::default()
        };
        let run_id = if dry_run {
            None
        } else {
            Some(self.catalog().start_run("import", self.trigger)?)
        };
        report.run_id = run_id;
        for (i, line) in BufReader::new(file).lines().enumerate() {
            let line_no = i + 1;
            let line = line.map_err(|e| PipelineError::Invalid(format!("entries.jsonl: {e}")))?;
            if line.trim().is_empty() {
                continue;
            }
            if self.is_cancelled() {
                report.stop = Some(Stop::Cancelled);
                break;
            }
            report.lines += 1;
            match self.prepare_record(bundle, &mapping, &line) {
                Ok(p) if dry_run => {
                    if p.raw_files.is_empty() {
                        report.raw_missing += 1;
                    }
                }
                Ok(p) => {
                    if let Err(e) = self.write_prepared(p, &mut report) {
                        report.invalid += 1;
                        report.errors.push(LineError {
                            line: line_no,
                            message: e.to_string(),
                        });
                    }
                }
                Err(message) => {
                    report.invalid += 1;
                    report.errors.push(LineError {
                        line: line_no,
                        message,
                    });
                }
            }
        }
        if let Some(id) = run_id {
            let status = match (&report.stop, report.invalid) {
                (Some(_), _) => RunStatus::Interrupted,
                (None, 0) => RunStatus::Ok,
                (None, _) => RunStatus::Partial,
            };
            self.catalog()
                .finish_run(id, status, &serde_json::to_value(&report)?, None)?;
        }
        Ok(report)
    }
}
