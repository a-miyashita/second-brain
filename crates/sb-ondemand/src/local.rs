//! `local.file`: a file on the local disk (source-documents.md).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sb_extract::bundle::{BundleInput, DocBundle, build_bundle, sha256_hex};
use second_brain_kernel::document::{IngestHint, IngestSettings, normalize_document};
use second_brain_kernel::source::{Source, SourceError, SyncHost};
use second_brain_kernel::{
    AccountCtx, AccountKind, FetchOutcome, FetchRequest, FetchedEntry, NormalizeCtx,
    NormalizeInput, NormalizeOutcome, SourceKind, SourceRef, SyncOptions,
};
use serde_json::{Map, Value, json};

use crate::glob;

pub struct LocalSource {
    account: AccountCtx,
    settings: IngestSettings,
    /// `$SECOND_BRAIN_HOME`: it holds the credentials, so it is never read.
    sb_home: PathBuf,
    /// The user's home directory, for the credential locations.
    user_home: Option<PathBuf>,
}

impl LocalSource {
    pub fn new(account: AccountCtx, settings: IngestSettings, sb_home: PathBuf) -> Self {
        let user_home = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf());
        LocalSource::with_user_home(account, settings, sb_home, user_home)
    }

    /// Like `new`, with an explicit user home (for tests).
    pub fn with_user_home(
        account: AccountCtx,
        settings: IngestSettings,
        sb_home: PathBuf,
        user_home: Option<PathBuf>,
    ) -> Self {
        LocalSource {
            account,
            settings,
            sb_home: canonical(&sb_home),
            user_home: user_home.map(|h| canonical(&h)),
        }
    }

    /// Built-in denied directories and files (credential locations). Each is also
    /// added in its canonical form, so a symlinked `~/.ssh` is covered.
    fn denied_roots(&self) -> Vec<PathBuf> {
        let mut v = vec![self.sb_home.clone()];
        if let Some(h) = &self.user_home {
            let mut rels: Vec<PathBuf> = [
                ".ssh",
                ".aws",
                ".gnupg",
                ".azure",
                ".kube",
                ".config/gcloud",
                ".config/gh",
                ".config/git/credentials",
                ".docker/config.json",
                ".netrc",
                ".npmrc",
                ".pypirc",
                ".git-credentials",
                ".password-store",
                ".local/share/keyrings",
                ".terraform.d/credentials.tfrc.json",
                "Library/Keychains",
            ]
            .iter()
            .map(PathBuf::from)
            .collect();
            if cfg!(windows) {
                rels.push(PathBuf::from("AppData").join("Roaming").join("gcloud"));
                rels.push(PathBuf::from("AppData").join("Roaming").join("gh"));
            }
            for rel in rels {
                let p = h.join(rel);
                let c = canonical(&p);
                if c != p {
                    v.push(c);
                }
                v.push(p);
            }
        }
        v
    }

    /// Whether a canonical path may not be ingested.
    pub fn is_denied(&self, path: &Path) -> bool {
        let lower = |p: &Path| {
            if cfg!(windows) {
                PathBuf::from(p.to_string_lossy().to_lowercase())
            } else {
                p.to_path_buf()
            }
        };
        let path_l = lower(path);
        if self
            .denied_roots()
            .iter()
            .any(|r| path_l.starts_with(lower(r)))
        {
            return true;
        }
        let text = path.to_string_lossy();
        BUILTIN_DENY_GLOBS
            .iter()
            .any(|pat| glob::matches(pat, &text))
            || self
                .settings
                .local_deny
                .iter()
                .any(|pat| glob::matches(pat, &text))
    }
}

/// Files that hold secrets wherever they are: environment files, private keys and
/// certificates, and credential files. There is no override (ADR-0014).
const BUILTIN_DENY_GLOBS: &[&str] = &[
    "**/.env",
    "**/.env.*",
    "**/*.pem",
    "**/*.key",
    "**/*.p12",
    "**/*.pfx",
    "**/id_rsa*",
    "**/id_dsa*",
    "**/id_ecdsa*",
    "**/id_ed25519*",
    "**/.netrc",
    "**/_netrc",
    "**/.npmrc",
    "**/.pypirc",
    "**/.git-credentials",
];

/// Canonicalize without the Windows `\\?\` prefix. A path that does not exist
/// is returned as given.
fn canonical(p: &Path) -> PathBuf {
    match std::fs::canonicalize(p) {
        Ok(c) => strip_verbatim(c),
        Err(_) => p.to_path_buf(),
    }
}

fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}

/// Expand `*` and `?` in the file name of a path, on Windows only (other shells
/// expand wildcards before the program starts). Files only, no recursion. A
/// pattern with no match, and anything that is not a plain path pattern, is
/// returned unchanged.
pub fn expand_wildcards(locator: &str) -> Vec<String> {
    if !cfg!(windows) || locator.contains("://") || !locator.contains(['*', '?']) {
        return vec![locator.to_string()];
    }
    let path = Path::new(locator);
    let (Some(dir), Some(pat)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return vec![locator.to_string()];
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| glob::matches(pat, n))
        })
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect();
    found.sort();
    if found.is_empty() {
        vec![locator.to_string()]
    } else {
        found
    }
}

fn file_url(path: &Path) -> Option<String> {
    url::Url::from_file_path(path).ok().map(String::from)
}

fn time(t: SystemTime) -> DateTime<Utc> {
    DateTime::<Utc>::from(t)
}

#[async_trait]
impl Source for LocalSource {
    fn kinds(&self) -> &'static [SourceKind] {
        &[SourceKind::LocalFile]
    }

    fn account_kind(&self) -> AccountKind {
        AccountKind::Local
    }

    fn supports_sync(&self) -> bool {
        false
    }

    fn refusal(&self, source_id: &str) -> Option<String> {
        let path = url::Url::parse(source_id).ok()?.to_file_path().ok()?;
        self.is_denied(&canonical(&path))
            .then(|| "path is not allowed".to_string())
    }

    /// The natural key: the canonical absolute path as a `file://` URL.
    fn resolve(&self, locator: &str) -> Option<SourceRef> {
        let path = match url::Url::parse(locator) {
            Ok(u) if u.scheme() == "file" => u.to_file_path().ok()?,
            _ => PathBuf::from(locator),
        };
        let canon = std::fs::canonicalize(&path).ok().map(strip_verbatim)?;
        let url = file_url(&canon)?;
        Some(SourceRef {
            account_id: self.account.id.clone(),
            source_kind: SourceKind::LocalFile,
            source_id: url.clone(),
            source_url: Some(url),
            created_at: None,
            updated_at: None,
        })
    }

    async fn sync(&self, _host: &dyn SyncHost, _opts: &SyncOptions) -> Result<(), SourceError> {
        Err(SourceError::Unsupported("local files have no sync".into()))
    }

    async fn fetch(
        &self,
        _host: &dyn SyncHost,
        req: &FetchRequest,
    ) -> Result<FetchOutcome, SourceError> {
        let path = url::Url::parse(&req.source_id)
            .ok()
            .and_then(|u| u.to_file_path().ok())
            .ok_or_else(|| SourceError::Parse(format!("not a file URL: {}", req.source_id)))?;
        let path = canonical(&path);
        if self.is_denied(&path) {
            return Err(SourceError::Rejected("path is not allowed".into()));
        }
        let meta = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(FetchOutcome::NotFound("file not found".into()));
            }
            Err(e) => return Err(SourceError::Api(format!("cannot read the file: {e}"))),
        };
        if meta.is_dir() {
            return Ok(FetchOutcome::NotApplicable(
                "a directory, not a file".into(),
            ));
        }
        if meta.len() > self.settings.max_file_bytes {
            return Err(SourceError::Rejected(format!(
                "the file is larger than ingest.max_file_bytes ({} bytes)",
                self.settings.max_file_bytes
            )));
        }
        let mtime = meta.modified().ok().map(time);
        let mtime_text = mtime.map(|t| t.to_rfc3339());
        let stored = req.fetch_state.as_ref();
        if !req.full
            && stored.is_some_and(|s| {
                s.get("size").and_then(Value::as_u64) == Some(meta.len())
                    && s.get("mtime").and_then(Value::as_str) == mtime_text.as_deref()
            })
        {
            return Ok(FetchOutcome::Unchanged);
        }
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| SourceError::Api(format!("cannot read the file: {e}")))?;
        let sha = sha256_hex(&bytes);
        if !req.full && stored.and_then(|s| s.get("sha256")).and_then(Value::as_str) == Some(&sha) {
            return Ok(FetchOutcome::Unchanged);
        }
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = Path::new(&file_name)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| file_name.clone());
        let hint = IngestHint::from_value(&req.hint);
        let created = meta.created().ok().map(time).or(mtime);
        let mut extra = Map::new();
        extra.insert("path".into(), json!(path.to_string_lossy()));
        extra.insert("file_name".into(), json!(file_name));
        extra.insert("size".into(), json!(meta.len()));
        extra.insert(
            "media_type".into(),
            json!(sb_extract::detect(&bytes, None, Some(&file_name)).as_str()),
        );
        if let Some(t) = &mtime_text {
            extra.insert("file_mtime".into(), json!(t));
        }
        let bundle = build_bundle(BundleInput {
            bytes: &bytes,
            original: None,
            media_type: None,
            file_name: Some(&file_name),
            hint: &hint,
            settings: &self.settings,
            fallback_title: Some(stem),
            source_created: created,
            source_modified: mtime,
            extra_meta: extra,
            fetch_state: Some(json!({
                "mtime": mtime_text,
                "size": meta.len(),
                "sha256": sha,
            })),
        })?;
        let url = file_url(&path).unwrap_or_else(|| req.source_id.clone());
        match bundle {
            DocBundle::NotApplicable(why) => Ok(FetchOutcome::NotApplicable(why)),
            DocBundle::Bundle(bundle) => Ok(FetchOutcome::Fetched(Box::new(FetchedEntry {
                source_ref: SourceRef {
                    account_id: self.account.id.clone(),
                    source_kind: SourceKind::LocalFile,
                    source_id: req.source_id.clone(),
                    source_url: Some(url),
                    created_at: created,
                    updated_at: mtime,
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

#[cfg(test)]
mod tests {
    use super::*;
    use second_brain_kernel::AccountId;

    fn source(deny: Vec<String>, sb_home: &Path, user_home: &Path) -> LocalSource {
        LocalSource::with_user_home(
            AccountCtx {
                id: AccountId::new("local").unwrap(),
                kind: AccountKind::Local,
                label: "Local files".into(),
                identity: None,
                config: json!({}),
            },
            IngestSettings {
                local_deny: deny,
                ..Default::default()
            },
            sb_home.to_path_buf(),
            Some(user_home.to_path_buf()),
        )
    }

    #[test]
    fn credential_locations_and_the_sb_home_are_denied() {
        let t = tempfile::tempdir().unwrap();
        let sb = t.path().join("sbhome");
        let user = t.path().join("user");
        std::fs::create_dir_all(sb.join("raw")).unwrap();
        std::fs::create_dir_all(user.join(".ssh")).unwrap();
        std::fs::create_dir_all(user.join("docs")).unwrap();
        let s = source(
            vec![format!("{}/**/*.pem", canonical(&user).display())],
            &sb,
            &user,
        );
        assert!(s.is_denied(&canonical(&sb).join("second-brain.db")));
        assert!(s.is_denied(&canonical(&user).join(".ssh").join("id_rsa")));
        assert!(!s.is_denied(&canonical(&user).join("docs").join("a.md")));
        assert!(s.is_denied(&canonical(&user).join("docs").join("key.pem")));
        assert!(!s.is_denied(&canonical(&user).join("docs").join("key.pem.txt")));
    }

    #[test]
    fn resolve_canonicalizes_and_rejects_missing_files() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("a.md");
        std::fs::write(&f, "hello").unwrap();
        let s = source(vec![], t.path(), t.path());
        let r = s.resolve(f.to_str().unwrap()).unwrap();
        assert!(r.source_id.starts_with("file://"));
        assert_eq!(r.source_url.as_deref(), Some(r.source_id.as_str()));
        let dotted = t.path().join(".").join("a.md");
        assert_eq!(
            s.resolve(dotted.to_str().unwrap()).unwrap().source_id,
            r.source_id
        );
        assert!(
            s.resolve(t.path().join("missing.md").to_str().unwrap())
                .is_none()
        );
    }
}
