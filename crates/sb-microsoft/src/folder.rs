//! Folder sync for SharePoint and OneDrive (docs/specs/folder-sync.md,
//! ADR-0022): the lister that [`second_brain_kernel::folder`] walks. A folder or
//! file is keyed `<driveId>/<itemId>`, like `microsoft.file` entries.

use async_trait::async_trait;
use second_brain_kernel::folder::{Child, FolderConfig, FolderFile, FolderLister};
use second_brain_kernel::source::SourceError;
use second_brain_kernel::util::parse_ts;
use serde_json::Value;

use crate::graph::{GraphApi, ItemLookup};
use crate::source::{str_at, unsupported_reason};

/// Lists drive folders for one account.
pub struct GraphFolderLister {
    api: GraphApi,
}

impl GraphFolderLister {
    pub fn new(api: GraphApi) -> Self {
        GraphFolderLister { api }
    }
}

/// `<driveId>/<itemId>` of an item. An item shared into the user's OneDrive
/// (`remoteItem`) is read through its target.
fn key_of(base: &Value) -> Option<String> {
    Some(format!(
        "{}/{}",
        str_at(base, "/parentReference/driveId")?,
        str_at(base, "/id")?
    ))
}

/// A child of a folder as `children` returned it.
pub(crate) fn child_of(item: &Value) -> Child {
    let remote = item.get("remoteItem").filter(|r| r.is_object());
    let base = remote.unwrap_or(item);
    if item.get("deleted").is_some() || base.get("deleted").is_some() {
        return Child::Unsupported;
    }
    if base.get("folder").is_some() {
        // A folder shared into OneDrive is not followed (folder-sync.md).
        if remote.is_some() {
            return Child::Unsupported;
        }
        return match key_of(base) {
            Some(id) => Child::Folder {
                id,
                name: str_at(base, "/name").unwrap_or("").to_string(),
            },
            None => Child::Unsupported,
        };
    }
    // Only items with a `file` facet are files (not OneNote packages, ...).
    if base.get("file").is_none() || base.get("package").is_some() {
        return Child::Unsupported;
    }
    let name = str_at(base, "/name").unwrap_or("");
    if unsupported_reason(name).is_some() {
        return Child::Unsupported;
    }
    let Some(id) = key_of(base) else {
        return Child::Unsupported;
    };
    let modified = str_at(base, "/lastModifiedDateTime").map(str::to_string);
    Child::File(FolderFile {
        id,
        name: name.to_string(),
        modified: modified.as_deref().and_then(parse_ts),
        modified_raw: modified,
        etag: str_at(base, "/eTag").map(str::to_string),
        size: base.get("size").and_then(Value::as_u64),
    })
}

#[async_trait]
impl FolderLister for GraphFolderLister {
    async fn root_name(&self, folder: &FolderConfig) -> Result<Option<String>, SourceError> {
        let Some((drive, item)) = folder.key.split_once('/') else {
            return Ok(None);
        };
        match self.api.item(drive, item).await? {
            ItemLookup::Missing { .. } => Ok(None),
            ItemLookup::Found(v) => {
                let base = v.get("remoteItem").filter(|r| r.is_object()).unwrap_or(&v);
                if v.get("deleted").is_some() || base.get("folder").is_none() {
                    return Ok(None);
                }
                Ok(Some(str_at(base, "/name").unwrap_or("").to_string()))
            }
        }
    }

    async fn children(&self, folder_id: &str) -> Result<Vec<Child>, SourceError> {
        let Some((drive, item)) = folder_id.split_once('/') else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut next: Option<String> = None;
        loop {
            let (items, n) = self.api.children_page(drive, item, next.as_deref()).await?;
            out.extend(items.iter().map(child_of));
            match n {
                Some(link) => next = Some(link),
                None => break,
            }
        }
        Ok(out)
    }
}

/// Whether a listed file differs from the state stored with its entry: the ETag
/// when both sides have one, else the modification time (as `fetch` compares).
pub fn changed(file: &FolderFile, state: &Value) -> bool {
    match (
        file.etag.as_deref(),
        state.get("etag").and_then(Value::as_str),
    ) {
        (Some(now), Some(then)) => now != then,
        _ => state.get("modified_time").and_then(Value::as_str) != file.modified_raw.as_deref(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(extra: Value) -> Value {
        let mut v = json!({"id": "I1", "name": "plan.docx", "eTag": "\"e1\"", "size": 10,
            "lastModifiedDateTime": "2026-09-01T00:00:00Z", "parentReference": {"driveId": "D1"},
            "file": {"mimeType": "x"}});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().cloned().unwrap());
        v
    }

    #[test]
    fn children_are_classified() {
        assert!(matches!(child_of(&item(json!({}))), Child::File(f)
            if f.id == "D1/I1" && f.etag.as_deref() == Some("\"e1\"") && f.size == Some(10)));
        let dir =
            json!({"id": "S", "name": "sub", "folder": {}, "parentReference": {"driveId": "D1"}});
        assert!(
            matches!(child_of(&dir), Child::Folder { id, name } if id == "D1/S" && name == "sub")
        );
        for unsupported in [
            item(json!({"deleted": {}})),
            item(json!({"package": {"type": "oneNote"}})),
            item(json!({"name": "notes.one"})),
            item(json!({"name": "old.doc"})),
            item(json!({"name": "photo.png"})),
            json!({"id": "X", "name": "thing", "parentReference": {"driveId": "D1"}}),
        ] {
            assert_eq!(child_of(&unsupported), Child::Unsupported, "{unsupported}");
        }
    }

    #[test]
    fn shared_items_are_followed_for_files_but_not_folders() {
        let remote_file = json!({"id": "R", "remoteItem": item(json!({"id": "T1", "parentReference": {"driveId": "D9"}}))});
        assert!(matches!(child_of(&remote_file), Child::File(f) if f.id == "D9/T1"));
        let remote_dir = json!({"id": "R", "remoteItem": {"id": "T2", "name": "d", "folder": {}, "parentReference": {"driveId": "D9"}}});
        assert_eq!(child_of(&remote_dir), Child::Unsupported);
    }

    #[test]
    fn change_detection_follows_fetch() {
        let f = FolderFile {
            id: "D/I".into(),
            name: "a".into(),
            modified: None,
            modified_raw: Some("t1".into()),
            etag: Some("e1".into()),
            size: None,
        };
        assert!(!changed(&f, &json!({"etag": "e1", "modified_time": "old"})));
        assert!(changed(&f, &json!({"etag": "e0", "modified_time": "t1"})));
        // No stored ETag: the time decides.
        assert!(!changed(&f, &json!({"modified_time": "t1"})));
        assert!(changed(&f, &json!({"modified_time": "t0"})));
    }
}
