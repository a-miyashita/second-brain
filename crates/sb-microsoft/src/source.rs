//! The `microsoft.file` source: SharePoint and OneDrive files added by `sb ingest`
//! (docs/specs/source-documents.md, ADR-0021). On demand only: no sync.

use async_trait::async_trait;
use second_brain_extract::bundle::{BundleInput, DocBundle, build_bundle};
use second_brain_kernel::document::{IngestHint, IngestSettings, normalize_document};
use second_brain_kernel::source::{Located, Source, SourceError, SyncHost};
use second_brain_kernel::util::parse_ts;
use second_brain_kernel::{
    AccountCtx, AccountKind, FetchOutcome, FetchRequest, FetchedEntry, NormalizeCtx,
    NormalizeInput, NormalizeOutcome, SourceKind, SourceRef, SyncOptions,
};
use serde_json::{Map, Value, json};

use crate::graph::{GraphApi, ItemLookup};

/// File types that are never worth a download.
const UNSUPPORTED_EXT: &[&str] = &[
    "one",
    "onetoc2",
    "loop",
    "whiteboard",
    "fluid",
    "aspx",
    "png",
    "jpg",
    "jpeg",
    "gif",
    "bmp",
    "tif",
    "tiff",
    "svg",
    "heic",
    "webp",
    "mp3",
    "wav",
    "m4a",
    "mp4",
    "mov",
    "avi",
    "mkv",
    "zip",
    "7z",
    "rar",
    "gz",
    "tar",
];

fn extension(name: &str) -> Option<String> {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())
}

/// What an item is, after following `remoteItem`.
#[derive(Debug, PartialEq)]
enum Kind {
    File(Box<Info>),
    NotApplicable(String),
    NotFound(String),
}

#[derive(Debug, PartialEq)]
struct Info {
    drive_id: String,
    item_id: String,
    name: String,
    size: Option<u64>,
    etag: Option<String>,
    mime: Option<String>,
    web_url: Option<String>,
    created: Option<String>,
    modified: Option<String>,
    drive_type: Option<String>,
    owners: Vec<Value>,
}

impl Info {
    fn key(&self) -> String {
        format!("{}/{}", self.drive_id, self.item_id)
    }
}

fn str_at<'a>(v: &'a Value, ptr: &str) -> Option<&'a str> {
    v.pointer(ptr)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn owners(item: &Value) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for who in ["/createdBy/user", "/lastModifiedBy/user"] {
        let name = str_at(item, &format!("{who}/displayName"));
        let email = str_at(item, &format!("{who}/email"));
        if name.is_none() && email.is_none() {
            continue;
        }
        let o = json!({"name": name, "email": email});
        if !out.contains(&o) {
            out.push(o);
        }
    }
    out
}

/// Read a drive item as `shares` or `items` returned it. An item shared into the
/// user's OneDrive (`remoteItem`) is followed to its target.
fn classify_item(item: &Value) -> Kind {
    let base = item
        .get("remoteItem")
        .filter(|r| r.is_object())
        .unwrap_or(item);
    if item.get("deleted").is_some() || base.get("deleted").is_some() {
        return Kind::NotFound("the item was deleted".into());
    }
    if base.get("folder").is_some() || item.get("folder").is_some() {
        return Kind::NotApplicable("folders are not supported".into());
    }
    if base.get("package").is_some() || item.get("package").is_some() {
        return Kind::NotApplicable("unsupported item type: package".into());
    }
    let (Some(item_id), Some(drive_id)) = (
        str_at(base, "/id"),
        str_at(base, "/parentReference/driveId"),
    ) else {
        return Kind::NotFound("the item has no drive or item ID".into());
    };
    Kind::File(Box::new(Info {
        drive_id: drive_id.to_string(),
        item_id: item_id.to_string(),
        name: str_at(base, "/name").unwrap_or("").to_string(),
        size: base.get("size").and_then(Value::as_u64),
        etag: str_at(base, "/eTag").map(str::to_string),
        mime: str_at(base, "/file/mimeType").map(str::to_string),
        web_url: str_at(base, "/webUrl").map(str::to_string),
        created: str_at(base, "/createdDateTime").map(str::to_string),
        modified: str_at(base, "/lastModifiedDateTime").map(str::to_string),
        drive_type: str_at(base, "/parentReference/driveType").map(str::to_string),
        owners: owners(base),
    }))
}

/// Why a file name is not worth downloading, if so.
fn unsupported_reason(name: &str) -> Option<String> {
    let ext = extension(name)?;
    match ext.as_str() {
        "doc" => Some("unsupported file type: legacy .doc (save it as docx)".into()),
        "ppt" => Some("unsupported file type: legacy .ppt (save it as pptx)".into()),
        e if UNSUPPORTED_EXT.contains(&e) => Some(format!("unsupported file type: .{e}")),
        _ => None,
    }
}

fn not_found_message(status: u16) -> String {
    if status == 400 {
        "Graph could not resolve this URL; copy a sharing link and ingest that".into()
    } else {
        "not found or no access".into()
    }
}

/// Ingest of SharePoint and OneDrive files for one account.
pub struct MicrosoftSource {
    account: AccountCtx,
    api: Option<GraphApi>,
    settings: IngestSettings,
}

impl MicrosoftSource {
    pub fn new(account: AccountCtx, api: Option<GraphApi>, settings: IngestSettings) -> Self {
        MicrosoftSource {
            account,
            api,
            settings,
        }
    }

    fn api(&self) -> Result<&GraphApi, SourceError> {
        self.api
            .as_ref()
            .ok_or_else(|| SourceError::Auth("no Microsoft credentials are stored".into()))
    }

    fn source_ref(&self, info: &Info) -> SourceRef {
        SourceRef {
            account_id: self.account.id.clone(),
            source_kind: SourceKind::MicrosoftFile,
            source_id: info.key(),
            source_url: info.web_url.clone(),
            created_at: info.created.as_deref().and_then(parse_ts),
            updated_at: info.modified.as_deref().and_then(parse_ts),
        }
    }
}

#[async_trait]
impl Source for MicrosoftSource {
    fn kinds(&self) -> &'static [SourceKind] {
        &[SourceKind::MicrosoftFile]
    }

    fn account_kind(&self) -> AccountKind {
        AccountKind::Microsoft
    }

    fn supports_sync(&self) -> bool {
        false
    }

    /// The key is known only to Graph, so a locator cannot be resolved offline.
    fn resolve(&self, _locator: &str) -> Option<SourceRef> {
        None
    }

    async fn locate(&self, locator: &str) -> Result<Located, SourceError> {
        match self.api()?.shares_item(locator).await? {
            ItemLookup::Missing { status, .. } => Ok(Located::NotFound(not_found_message(status))),
            ItemLookup::Found(item) => Ok(match classify_item(&item) {
                Kind::File(info) => Located::Found(self.source_ref(&info)),
                Kind::NotApplicable(why) => Located::NotApplicable(why),
                Kind::NotFound(why) => Located::NotFound(why),
            }),
        }
    }

    async fn sync(&self, _host: &dyn SyncHost, _opts: &SyncOptions) -> Result<(), SourceError> {
        Err(SourceError::Unsupported(
            "SharePoint and OneDrive files have no sync".into(),
        ))
    }

    async fn fetch(
        &self,
        _host: &dyn SyncHost,
        req: &FetchRequest,
    ) -> Result<FetchOutcome, SourceError> {
        let api = self.api()?;
        let Some((drive_id, item_id)) = req.source_id.split_once('/') else {
            return Ok(FetchOutcome::NotFound("not a drive item key".into()));
        };
        let item = match api.item(drive_id, item_id).await? {
            ItemLookup::Found(v) => v,
            ItemLookup::Missing { status, .. } => {
                return Ok(FetchOutcome::NotFound(not_found_message(status)));
            }
        };
        let info = match classify_item(&item) {
            Kind::File(i) => i,
            Kind::NotApplicable(why) => return Ok(FetchOutcome::NotApplicable(why)),
            Kind::NotFound(why) => return Ok(FetchOutcome::NotFound(why)),
        };
        let stored = |k: &str| {
            req.fetch_state
                .as_ref()
                .and_then(|s| s.get(k))
                .and_then(Value::as_str)
        };
        let same = match (&info.etag, stored("etag")) {
            (Some(now), Some(then)) => now == then,
            _ => info
                .modified
                .as_deref()
                .is_some_and(|m| stored("modified_time") == Some(m)),
        };
        if !req.full && info.key() == req.source_id && same {
            return Ok(FetchOutcome::Unchanged);
        }
        if let Some(why) = unsupported_reason(&info.name) {
            return Ok(FetchOutcome::NotApplicable(why));
        }
        if info.size.is_some_and(|s| s > self.settings.max_file_bytes) {
            return Err(SourceError::Rejected(format!(
                "the file is larger than ingest.max_file_bytes ({} bytes)",
                self.settings.max_file_bytes
            )));
        }
        let Some(raw) = api
            .download(&info.drive_id, &info.item_id, self.settings.max_file_bytes)
            .await?
        else {
            return Ok(FetchOutcome::NotFound("download failed: not found".into()));
        };
        let hint = IngestHint::from_value(&req.hint);
        let created = info.created.as_deref().and_then(parse_ts);
        let updated = info.modified.as_deref().and_then(parse_ts);
        let mut extra = Map::new();
        extra.insert("drive_id".into(), json!(info.drive_id));
        extra.insert("item_id".into(), json!(info.item_id));
        extra.insert("drive_type".into(), json!(info.drive_type));
        extra.insert("mime_type".into(), json!(info.mime));
        extra.insert("file_name".into(), json!(info.name));
        extra.insert("owners".into(), json!(info.owners));
        extra.insert("ms_modified_time".into(), json!(info.modified));
        extra.insert("ms_etag".into(), json!(info.etag));
        let stem = match info.name.rsplit_once('.') {
            Some((s, _)) if !s.is_empty() => s.to_string(),
            _ => info.name.clone(),
        };
        let bundle = build_bundle(BundleInput {
            bytes: &raw,
            original: None,
            media_type: info.mime.as_deref(),
            file_name: Some(&info.name),
            hint: &hint,
            settings: &self.settings,
            fallback_title: Some(stem),
            source_created: created,
            source_modified: updated,
            extra_meta: extra,
            fetch_state: Some(json!({"etag": info.etag, "modified_time": info.modified})),
        })?;
        match bundle {
            DocBundle::NotApplicable(why) => Ok(FetchOutcome::NotApplicable(why)),
            DocBundle::Bundle(bundle) => Ok(FetchOutcome::Fetched(Box::new(FetchedEntry {
                source_ref: self.source_ref(&info),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn file(extra: Value) -> Value {
        let mut v = json!({
            "id": "ITEM1", "name": "Plan.docx", "size": 10, "eTag": "\"e1\"",
            "webUrl": "https://contoso.example/Plan.docx",
            "createdDateTime": "2026-01-02T03:04:05Z",
            "lastModifiedDateTime": "2026-02-03T04:05:06Z",
            "file": {"mimeType": "application/x"},
            "parentReference": {"driveId": "b!DRIVE", "driveType": "documentLibrary"},
            "createdBy": {"user": {"displayName": "A", "email": "a@example.test"}},
            "lastModifiedBy": {"user": {"displayName": "A", "email": "a@example.test"}}
        });
        for (k, x) in extra.as_object().cloned().unwrap_or_default() {
            v[k] = x;
        }
        v
    }

    #[test]
    fn a_file_is_keyed_by_drive_and_item() {
        let Kind::File(i) = classify_item(&file(json!({}))) else {
            panic!("not a file");
        };
        assert_eq!(i.key(), "b!DRIVE/ITEM1");
        assert_eq!(i.drive_type.as_deref(), Some("documentLibrary"));
        assert_eq!(i.owners.len(), 1, "the same person is listed once");
    }

    #[test]
    fn a_remote_item_is_followed_to_its_target() {
        let v = json!({
            "id": "LOCAL", "parentReference": {"driveId": "mine"},
            "remoteItem": {"id": "TARGET", "name": "T.pdf",
                "parentReference": {"driveId": "theirs"}, "file": {"mimeType": "application/pdf"}}
        });
        let Kind::File(i) = classify_item(&v) else {
            panic!("not a file");
        };
        assert_eq!(i.key(), "theirs/TARGET");
    }

    #[test]
    fn folders_packages_and_deleted_items_are_not_files() {
        assert!(matches!(
            classify_item(&file(json!({"folder": {}}))),
            Kind::NotApplicable(m) if m.contains("folders")
        ));
        assert!(matches!(
            classify_item(&file(json!({"package": {"type": "oneNote"}}))),
            Kind::NotApplicable(m) if m.contains("package")
        ));
        assert!(matches!(
            classify_item(&file(json!({"deleted": {}}))),
            Kind::NotFound(_)
        ));
        assert!(matches!(
            classify_item(&json!({"id": "x"})),
            Kind::NotFound(_)
        ));
    }

    #[test]
    fn unsupported_names() {
        assert!(unsupported_reason("a.docx").is_none());
        assert!(unsupported_reason("README").is_none());
        assert!(
            unsupported_reason("old.DOC")
                .unwrap()
                .contains("save it as docx")
        );
        assert!(unsupported_reason("deck.ppt").unwrap().contains("pptx"));
        assert!(unsupported_reason("notes.one").is_some());
        assert!(unsupported_reason("Page.aspx").is_some());
        assert!(unsupported_reason("clip.mp4").is_some());
    }
}
