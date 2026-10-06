//! The `google.doc` source: Google Docs, Sheets, Slides and other Drive files
//! added by `sb ingest` (source-documents.md, ADR-0014), and the account's
//! combined source adapter.

use async_trait::async_trait;
use sb_core::document::{IngestHint, IngestSettings, normalize_document};
use sb_core::source::{Source, SourceError, SyncHost};
use sb_core::util::parse_ts;
use sb_core::{
    AccountCtx, AccountKind, FetchOutcome, FetchRequest, FetchedEntry, NormalizeCtx,
    NormalizeInput, NormalizeOutcome, SourceKind, SourceRef, SyncOptions,
};
use sb_extract::bundle::{BundleInput, DocBundle, build_bundle};
use serde_json::{Map, Value, json};

use crate::api::{GOOGLE_DOC_MIME, GoogleApi, doc_id_from_url};
use crate::gemini;
use crate::meet::MeetSource;

const SHEET_MIME: &str = "application/vnd.google-apps.spreadsheet";
const SLIDES_MIME: &str = "application/vnd.google-apps.presentation";
const SHORTCUT_MIME: &str = "application/vnd.google-apps.shortcut";
const XLSX_MIME: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const PPTX_MIME: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation";

/// How a Drive file is read.
enum Plan {
    /// A native Google file exported in another format.
    Export {
        mime: &'static str,
        ext: &'static str,
        format: &'static str,
    },
    /// A regular file downloaded as it is.
    Download,
    Unsupported,
}

fn plan(mime: &str) -> Plan {
    match mime {
        GOOGLE_DOC_MIME => Plan::Export {
            mime: "text/markdown",
            ext: "md",
            format: "markdown",
        },
        SHEET_MIME => Plan::Export {
            mime: XLSX_MIME,
            ext: "xlsx",
            format: "xlsx",
        },
        SLIDES_MIME => Plan::Export {
            mime: PPTX_MIME,
            ext: "pptx",
            format: "pptx",
        },
        m if m.starts_with("application/vnd.google-apps.") => Plan::Unsupported,
        "application/pdf"
        | "application/json"
        | "application/vnd.ms-excel"
        | "application/vnd.oasis.opendocument.spreadsheet"
        | XLSX_MIME
        | PPTX_MIME
        | "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            Plan::Download
        }
        m if m.starts_with("text/") => Plan::Download,
        _ => Plan::Unsupported,
    }
}

/// The Drive file ID of a locator: a Docs URL, or `open?id=` / `uc?id=`.
fn file_id(locator: &str) -> Option<String> {
    if let Some(id) = doc_id_from_url(locator) {
        return Some(id);
    }
    let u = url::Url::parse(locator).ok()?;
    u.query_pairs()
        .find(|(k, _)| k == "id")
        .map(|(_, v)| v.into_owned())
        .filter(|v| !v.is_empty())
}

fn forbidden(e: &SourceError) -> bool {
    matches!(e, SourceError::Api(m) if m.starts_with("Google API 403"))
}

fn too_large_to_export(e: &SourceError) -> bool {
    matches!(e, SourceError::Api(m)
        if m.starts_with("Google API 403") && m.to_ascii_lowercase().contains("too large"))
}

/// Ingest of Drive files for one account.
pub struct DocSource {
    account: AccountCtx,
    api: Option<GoogleApi>,
    settings: IngestSettings,
}

impl DocSource {
    pub fn new(account: AccountCtx, api: Option<GoogleApi>, settings: IngestSettings) -> Self {
        DocSource {
            account,
            api,
            settings,
        }
    }

    fn api(&self) -> Result<&GoogleApi, SourceError> {
        self.api
            .as_ref()
            .ok_or_else(|| SourceError::Auth("no Google credentials are stored".into()))
    }

    /// Metadata of a file, following one level of shortcut. `Ok(Err(outcome))`
    /// is a result the caller returns as it is.
    async fn details(&self, id: &str) -> Result<Result<Value, FetchOutcome>, SourceError> {
        let api = self.api()?;
        let mut current = id.to_string();
        for _ in 0..2 {
            let file = match api.file_details(&current).await {
                Ok(Some(f)) => f,
                Ok(None) => {
                    return Ok(Err(FetchOutcome::NotFound("not found or no access".into())));
                }
                Err(e) if forbidden(&e) => {
                    return Ok(Err(FetchOutcome::NotFound("not found or no access".into())));
                }
                Err(e) => return Err(e),
            };
            if file.get("trashed").and_then(Value::as_bool) == Some(true) {
                return Ok(Err(FetchOutcome::NotFound(
                    "the file is in the trash".into(),
                )));
            }
            if file.get("mimeType").and_then(Value::as_str) == Some(SHORTCUT_MIME) {
                match file
                    .pointer("/shortcutDetails/targetId")
                    .and_then(Value::as_str)
                {
                    Some(t) => {
                        current = t.to_string();
                        continue;
                    }
                    None => {
                        return Ok(Err(FetchOutcome::NotApplicable(
                            "a shortcut without a target".into(),
                        )));
                    }
                }
            }
            return Ok(Ok(file));
        }
        Ok(Err(FetchOutcome::NotApplicable(
            "a shortcut to a shortcut".into(),
        )))
    }
}

#[async_trait]
impl Source for DocSource {
    fn kinds(&self) -> &'static [SourceKind] {
        &[SourceKind::GoogleDoc]
    }

    fn account_kind(&self) -> AccountKind {
        AccountKind::Google
    }

    fn supports_sync(&self) -> bool {
        false
    }

    fn resolve(&self, locator: &str) -> Option<SourceRef> {
        let id = file_id(locator)?;
        Some(SourceRef {
            account_id: self.account.id.clone(),
            source_kind: SourceKind::GoogleDoc,
            source_url: Some(format!("https://docs.google.com/document/d/{id}/edit")),
            source_id: id,
            created_at: None,
            updated_at: None,
        })
    }

    async fn sync(&self, _host: &dyn SyncHost, _opts: &SyncOptions) -> Result<(), SourceError> {
        Err(SourceError::Unsupported("Drive files have no sync".into()))
    }

    async fn fetch(
        &self,
        _host: &dyn SyncHost,
        req: &FetchRequest,
    ) -> Result<FetchOutcome, SourceError> {
        let api = self.api()?;
        let file = match self.details(&req.source_id).await? {
            Ok(f) => f,
            Err(outcome) => return Ok(outcome),
        };
        let id = file
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(&req.source_id)
            .to_string();
        let mime = file.get("mimeType").and_then(Value::as_str).unwrap_or("");
        let name = file
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let modified = file
            .get("modifiedTime")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let stored = req
            .fetch_state
            .as_ref()
            .and_then(|s| s.get("modified_time"))
            .and_then(Value::as_str);
        if !req.full && id == req.source_id && !modified.is_empty() && stored == Some(&modified) {
            return Ok(FetchOutcome::Unchanged);
        }
        let plan = plan(mime);
        let size: Option<u64> = file
            .get("size")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok());
        let (raw, media_type, file_name, format): (Vec<u8>, String, String, &str) = match plan {
            Plan::Unsupported => {
                return Ok(FetchOutcome::NotApplicable(format!(
                    "unsupported Drive file type: {mime}"
                )));
            }
            Plan::Export {
                mime: export_mime,
                ext,
                format,
            } => {
                let bytes = match api.export(&id, export_mime).await {
                    Ok(Some(b)) => b,
                    Ok(None) => {
                        return Ok(FetchOutcome::NotFound("export failed: not found".into()));
                    }
                    Err(e) if too_large_to_export(&e) => {
                        return Err(SourceError::Rejected(
                            "Drive refuses to export a file this large (the limit is 10 MB)".into(),
                        ));
                    }
                    Err(e) => return Err(e),
                };
                (
                    bytes,
                    export_mime.to_string(),
                    format!("{name}.{ext}"),
                    format,
                )
            }
            Plan::Download => {
                if size.is_some_and(|s| s > self.settings.max_file_bytes) {
                    return Err(SourceError::Rejected(format!(
                        "the file is larger than ingest.max_file_bytes ({} bytes)",
                        self.settings.max_file_bytes
                    )));
                }
                let Some(bytes) = api.download(&id, self.settings.max_file_bytes).await? else {
                    return Ok(FetchOutcome::NotFound("download failed: not found".into()));
                };
                (bytes, mime.to_string(), name.clone(), "original")
            }
        };
        // Native Docs come as Markdown with Google's noise; the cleanup is the
        // one the Gemini notes parser uses. The export itself is the original.
        let cleaned;
        let text_bytes: &[u8] = if mime == GOOGLE_DOC_MIME {
            cleaned = gemini::clean(&String::from_utf8_lossy(&raw));
            cleaned.as_bytes()
        } else {
            &raw
        };
        let hint = IngestHint::from_value(&req.hint);
        let created = file
            .get("createdTime")
            .and_then(Value::as_str)
            .and_then(parse_ts);
        let updated = parse_ts(&modified);
        let owners: Vec<Value> = file
            .get("owners")
            .and_then(Value::as_array)
            .map(|o| {
                o.iter()
                    .map(|p| {
                        json!({
                            "name": p.get("displayName"),
                            "email": p.get("emailAddress"),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut extra = Map::new();
        extra.insert("drive_file_id".into(), json!(id));
        extra.insert("mime_type".into(), json!(mime));
        extra.insert("owners".into(), json!(owners));
        extra.insert("drive_modified_time".into(), json!(modified));
        extra.insert("export_format".into(), json!(format));
        let bundle = build_bundle(BundleInput {
            bytes: text_bytes,
            original: Some(&raw),
            media_type: Some(&media_type),
            file_name: Some(&file_name),
            hint: &hint,
            settings: &self.settings,
            fallback_title: Some(name),
            source_created: created,
            source_modified: updated,
            extra_meta: extra,
            fetch_state: Some(json!({"modified_time": modified})),
        })?;
        match bundle {
            DocBundle::NotApplicable(why) => Ok(FetchOutcome::NotApplicable(why)),
            DocBundle::Bundle(bundle) => Ok(FetchOutcome::Fetched(Box::new(FetchedEntry {
                source_ref: SourceRef {
                    account_id: self.account.id.clone(),
                    source_kind: SourceKind::GoogleDoc,
                    source_id: id.clone(),
                    source_url: file
                        .get("webViewLink")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| Some(format!("https://docs.google.com/document/d/{id}/edit"))),
                    created_at: created,
                    updated_at: updated,
                },
                bundle,
            }))),
        }
    }

    fn normalize(
        &self,
        _ctx: &NormalizeCtx,
        input: &NormalizeInput,
    ) -> Result<NormalizeOutcome, SourceError> {
        normalize_document(&input.source_ref, &input.fetch_metadata, &input.segments)
    }
}

/// The adapter of a Google account: Meet notes (with sync) and Drive files
/// (on demand), dispatched by source kind.
pub struct GoogleSource {
    meet: MeetSource,
    doc: DocSource,
}

impl GoogleSource {
    pub fn new(
        account: AccountCtx,
        api: Option<GoogleApi>,
        settings: IngestSettings,
    ) -> Result<Self, SourceError> {
        Ok(GoogleSource {
            meet: MeetSource::new(account.clone(), api.clone())?,
            doc: DocSource::new(account, api, settings),
        })
    }
}

#[async_trait]
impl Source for GoogleSource {
    /// The kinds that `sync` covers.
    fn kinds(&self) -> &'static [SourceKind] {
        self.meet.kinds()
    }

    fn account_kind(&self) -> AccountKind {
        AccountKind::Google
    }

    fn supports_sync(&self) -> bool {
        self.meet.supports_sync()
    }

    fn resolve(&self, locator: &str) -> Option<SourceRef> {
        self.doc.resolve(locator)
    }

    async fn sync(&self, host: &dyn SyncHost, opts: &SyncOptions) -> Result<(), SourceError> {
        self.meet.sync(host, opts).await
    }

    async fn fetch(
        &self,
        host: &dyn SyncHost,
        req: &FetchRequest,
    ) -> Result<FetchOutcome, SourceError> {
        match req.source_kind {
            SourceKind::GoogleDoc => self.doc.fetch(host, req).await,
            _ => self.meet.fetch(host, req).await,
        }
    }

    fn load_snapshot(&self, host: &dyn SyncHost) -> Result<Value, SourceError> {
        self.meet.load_snapshot(host)
    }

    fn normalize(
        &self,
        ctx: &NormalizeCtx,
        input: &NormalizeInput,
    ) -> Result<NormalizeOutcome, SourceError> {
        match input.source_ref.source_kind {
            SourceKind::GoogleDoc => self.doc.normalize(ctx, input),
            _ => self.meet.normalize(ctx, input),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locators_and_plans() {
        assert_eq!(
            file_id("https://docs.google.com/document/d/1AbC-_9/edit"),
            Some("1AbC-_9".into())
        );
        assert_eq!(
            file_id("https://drive.google.com/open?id=OPEN1"),
            Some("OPEN1".into())
        );
        assert_eq!(file_id("https://example.test/"), None);
        assert!(matches!(
            plan(GOOGLE_DOC_MIME),
            Plan::Export { ext: "md", .. }
        ));
        assert!(matches!(plan(SHEET_MIME), Plan::Export { ext: "xlsx", .. }));
        assert!(matches!(
            plan(SLIDES_MIME),
            Plan::Export { ext: "pptx", .. }
        ));
        assert!(matches!(plan("application/pdf"), Plan::Download));
        assert!(matches!(plan("text/csv"), Plan::Download));
        assert!(matches!(
            plan("application/vnd.google-apps.form"),
            Plan::Unsupported
        ));
        assert!(matches!(plan("image/png"), Plan::Unsupported));
    }
}
