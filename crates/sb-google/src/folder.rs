//! Folder sync for Google Drive (docs/specs/folder-sync.md, ADR-0022): the
//! lister that [`second_brain_kernel::folder`] walks.

use async_trait::async_trait;
use second_brain_kernel::SourceKind;
use second_brain_kernel::folder::{
    Child, FolderConfig, FolderFile, FolderLister, FolderOps, ResolvedFolder,
};
use second_brain_kernel::source::SourceError;
use second_brain_kernel::util::parse_ts;
use serde_json::Value;

use crate::api::{FOLDER_MIME, GoogleApi};
use crate::doc::{Plan, SHORTCUT_MIME, plan};

/// Lists Drive folders for one account.
pub struct DriveFolderLister {
    api: GoogleApi,
}

impl DriveFolderLister {
    pub fn new(api: GoogleApi) -> Self {
        DriveFolderLister { api }
    }
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// A file or folder as Drive lists it, as a child. A shortcut to a file is
/// followed (its target is the entry), a shortcut to a folder is not.
fn child_of(item: &Value, target: Option<&Value>) -> Child {
    let item = target.unwrap_or(item);
    let mime = str_of(item, "mimeType");
    if mime == FOLDER_MIME {
        return Child::Folder {
            id: str_of(item, "id").to_string(),
            name: str_of(item, "name").to_string(),
        };
    }
    if matches!(plan(mime), Plan::Unsupported) {
        return Child::Unsupported;
    }
    let modified = item
        .get("modifiedTime")
        .and_then(Value::as_str)
        .map(str::to_string);
    Child::File(FolderFile {
        id: str_of(item, "id").to_string(),
        name: str_of(item, "name").to_string(),
        modified: modified.as_deref().and_then(parse_ts),
        modified_raw: modified,
        etag: None,
        size: item
            .get("size")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok()),
    })
}

#[async_trait]
impl FolderLister for DriveFolderLister {
    async fn root_name(&self, folder: &FolderConfig) -> Result<Option<String>, SourceError> {
        let Some(f) = self.api.file_details(&folder.key).await? else {
            return Ok(None);
        };
        if f.get("trashed").and_then(Value::as_bool) == Some(true)
            || str_of(&f, "mimeType") != FOLDER_MIME
        {
            return Ok(None);
        }
        Ok(Some(str_of(&f, "name").to_string()))
    }

    async fn children(&self, folder_id: &str) -> Result<Vec<Child>, SourceError> {
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let (items, next) = self.api.list_children(folder_id, page.as_deref()).await?;
            for item in &items {
                if str_of(item, "mimeType") == SHORTCUT_MIME {
                    let det = item.get("shortcutDetails");
                    let target_mime = det.map(|d| str_of(d, "targetMimeType")).unwrap_or("");
                    let target_id = det.map(|d| str_of(d, "targetId")).unwrap_or("");
                    if target_id.is_empty() || target_mime == FOLDER_MIME {
                        out.push(Child::Unsupported);
                        continue;
                    }
                    // The shortcut has its own times; the entry is the target's.
                    match self.api.file(target_id).await? {
                        Some(t) => out.push(child_of(item, Some(&t))),
                        None => out.push(Child::Unsupported),
                    }
                    continue;
                }
                out.push(child_of(item, None));
            }
            match next {
                Some(t) => page = Some(t),
                None => break,
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl FolderOps for DriveFolderLister {
    fn kind(&self) -> SourceKind {
        SourceKind::GoogleDoc
    }

    fn duplicate_kinds(&self) -> &'static [SourceKind] {
        &[SourceKind::GoogleMeet]
    }

    fn changed(&self, file: &FolderFile, state: &Value) -> bool {
        changed(file, state)
    }

    async fn resolve_folder(&self, url: &str) -> Result<ResolvedFolder, SourceError> {
        let Some(id) = folder_id_from_url(url) else {
            return Err(SourceError::Rejected(
                "not a Google Drive folder URL (expected https://drive.google.com/drive/folders/<id>); \
                 use `sb ingest` for a file"
                    .into(),
            ));
        };
        let Some(mut f) = self.api.file_details(&id).await? else {
            return Err(SourceError::Rejected(
                "the folder was not found, or this account cannot read it".into(),
            ));
        };
        // A shortcut is followed to its target.
        if str_of(&f, "mimeType") == SHORTCUT_MIME
            && let Some(target) = f
                .pointer("/shortcutDetails/targetId")
                .and_then(Value::as_str)
        {
            f = self.api.file_details(target).await?.ok_or_else(|| {
                SourceError::Rejected("the shortcut's target cannot be read".into())
            })?;
        }
        if str_of(&f, "mimeType") != FOLDER_MIME {
            return Err(SourceError::Rejected(
                "that is a file, not a folder; use `sb ingest` for a file".into(),
            ));
        }
        if f.get("trashed").and_then(Value::as_bool) == Some(true) {
            return Err(SourceError::Rejected("the folder is in the trash".into()));
        }
        Ok(ResolvedFolder {
            key: str_of(&f, "id").to_string(),
            name: str_of(&f, "name").to_string(),
        })
    }
}

/// Whether a listed Drive file differs from the state stored with its entry.
pub fn changed(file: &FolderFile, state: &Value) -> bool {
    state.get("modified_time").and_then(Value::as_str) != file.modified_raw.as_deref()
}

/// The folder ID in a Drive folder URL (`/drive/folders/<id>`, also with
/// `/u/<n>/` or `/a/<domain>` prefixes, and `open?id=<id>`).
pub fn folder_id_from_url(url: &str) -> Option<String> {
    let u = url::Url::parse(url.trim()).ok()?;
    if u.host_str() != Some("drive.google.com") {
        return None;
    }
    let segs: Vec<&str> = u.path_segments()?.collect();
    if let Some(i) = segs.iter().position(|s| *s == "folders") {
        let id = segs.get(i + 1)?;
        return (!id.is_empty()).then(|| (*id).to_string());
    }
    if matches!(u.path(), "/open" | "/uc") {
        return u
            .query_pairs()
            .find(|(k, _)| k == "id")
            .map(|(_, v)| v.into_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn folder_urls() {
        for u in [
            "https://drive.google.com/drive/folders/1AbC_d-9",
            "https://drive.google.com/drive/folders/1AbC_d-9?usp=sharing",
            "https://drive.google.com/drive/u/0/folders/1AbC_d-9",
            "https://drive.google.com/open?id=1AbC_d-9",
        ] {
            assert_eq!(folder_id_from_url(u).as_deref(), Some("1AbC_d-9"), "{u}");
        }
        assert_eq!(
            folder_id_from_url("https://drive.google.com/file/d/X/view"),
            None
        );
        assert_eq!(
            folder_id_from_url("https://example.com/drive/folders/X"),
            None
        );
    }

    #[test]
    fn children_are_classified() {
        let doc = json!({"id": "D", "name": "n", "mimeType": "application/vnd.google-apps.document", "modifiedTime": "2026-09-01T00:00:00Z"});
        assert!(
            matches!(child_of(&doc, None), Child::File(f) if f.id == "D" && f.modified.is_some())
        );
        let form = json!({"id": "F", "mimeType": "application/vnd.google-apps.form"});
        assert_eq!(child_of(&form, None), Child::Unsupported);
        let img = json!({"id": "I", "mimeType": "image/png"});
        assert_eq!(child_of(&img, None), Child::Unsupported);
        let dir = json!({"id": "S", "name": "sub", "mimeType": FOLDER_MIME});
        assert!(matches!(child_of(&dir, None), Child::Folder { id, .. } if id == "S"));
        let pdf =
            json!({"id": "P", "name": "a.pdf", "mimeType": "application/pdf", "size": "1200"});
        assert!(matches!(child_of(&pdf, None), Child::File(f) if f.size == Some(1200)));
    }
}
