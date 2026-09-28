//! Raw files under `$SECOND_BRAIN_HOME/raw/` (ADR-0002, ADR-0012). Files are
//! written to `tmp/` and renamed into place; they are never modified after.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Utc};
use sb_core::util::{sha256_hex, slugify_source_id};
use sb_core::{RawObject, RawRole, RawSegment, SourceRef};

use crate::entries::{RawObjectRow, StoredRaw};
use crate::error::{Result, StoreError};
use crate::home::Home;

/// The relative directory for an entry's raw files.
pub fn entry_dir(r: &SourceRef, fallback_date: DateTime<Utc>) -> String {
    let d = r.created_at.unwrap_or(fallback_date);
    format!(
        "raw/{}/{}/{:04}/{:02}/{}",
        r.account_id,
        r.source_kind,
        d.year(),
        d.month(),
        slugify_source_id(&r.source_id)
    )
}

/// The directory of an existing raw path (`raw/.../<slug>/<file>` → `raw/.../<slug>`).
pub fn dir_of(rel: &str) -> String {
    rel.rsplit_once('/')
        .map(|(d, _)| d.to_string())
        .unwrap_or_default()
}

fn safe_ext(ext: &str) -> String {
    let e: String = ext
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(10)
        .collect();
    if e.is_empty() {
        "bin".into()
    } else {
        e.to_lowercase()
    }
}

/// Write one object atomically at `<dir>/<role>.<seq>.<ext>`.
pub fn write_object(home: &Home, dir_rel: &str, seq: i64, obj: &RawObject) -> Result<StoredRaw> {
    let rel = format!("{dir_rel}/{}.{seq}.{}", obj.role, safe_ext(&obj.ext));
    let dest = home.resolve_rel(&rel);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
    }
    let tmp_dir = home.tmp_dir();
    fs::create_dir_all(&tmp_dir).map_err(|e| StoreError::io(&tmp_dir, e))?;
    let tmp = tmp_dir.join(format!("raw-{}.part", ulid::Ulid::new()));
    {
        let mut f = fs::File::create(&tmp).map_err(|e| StoreError::io(&tmp, e))?;
        f.write_all(&obj.bytes)
            .map_err(|e| StoreError::io(&tmp, e))?;
        f.sync_all().map_err(|e| StoreError::io(&tmp, e))?;
    }
    if let Err(e) = fs::rename(&tmp, &dest) {
        let _ = fs::remove_file(&tmp);
        return Err(StoreError::io(&dest, e));
    }
    Ok(StoredRaw {
        role: obj.role,
        seq,
        path: rel,
        media_type: obj.media_type.clone(),
        sha256: sha256_hex(&obj.bytes),
        size: obj.bytes.len() as i64,
    })
}

/// Read stored segments into memory, ordered by role then seq.
pub fn read_segments(home: &Home, rows: &[RawObjectRow]) -> Result<Vec<RawSegment>> {
    let mut rows: Vec<&RawObjectRow> = rows.iter().collect();
    rows.sort_by(|a, b| (a.role.as_str(), a.seq).cmp(&(b.role.as_str(), b.seq)));
    rows.into_iter()
        .map(|r| {
            let p = home.resolve_rel(&r.path);
            let bytes = fs::read(&p).map_err(|e| StoreError::io(&p, e))?;
            Ok(RawSegment {
                role: r.role,
                seq: r.seq,
                media_type: r.media_type.clone(),
                bytes,
            })
        })
        .collect()
}

/// Next segment number for a role.
pub fn next_seq(rows: &[RawObjectRow], role: RawRole) -> i64 {
    rows.iter()
        .filter(|r| r.role == role)
        .map(|r| r.seq + 1)
        .max()
        .unwrap_or(0)
}

/// Delete raw files and prune empty parent directories up to `raw/`.
pub fn delete_paths(home: &Home, paths: &[String]) {
    for rel in paths {
        let p = home.resolve_rel(rel);
        if let Err(e) = fs::remove_file(&p)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %p.display(), error = %e, "could not delete raw file");
        }
        prune_empty_dirs(home, &p);
    }
}

fn prune_empty_dirs(home: &Home, file: &Path) {
    let raw = home.raw_dir();
    let mut dir = file.parent().map(Path::to_path_buf);
    while let Some(d) = dir {
        if d == raw || !d.starts_with(&raw) {
            break;
        }
        if fs::remove_dir(&d).is_err() {
            break;
        }
        dir = d.parent().map(Path::to_path_buf);
    }
}

/// Files under `raw/` and `tmp/` not referenced by `referenced` (relative paths).
pub fn find_orphans(home: &Home, referenced: &[String]) -> Result<Vec<PathBuf>> {
    let refs: std::collections::HashSet<PathBuf> =
        referenced.iter().map(|r| home.resolve_rel(r)).collect();
    let mut out = Vec::new();
    for root in [home.raw_dir(), home.tmp_dir()] {
        walk(&root, &mut |p| {
            if !refs.contains(p) {
                out.push(p.to_path_buf());
            }
        })?;
    }
    Ok(out)
}

fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) -> Result<()> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(StoreError::io(dir, e)),
    };
    for ent in rd {
        let ent = ent.map_err(|e| StoreError::io(dir, e))?;
        let p = ent.path();
        let ft = ent.file_type().map_err(|e| StoreError::io(&p, e))?;
        if ft.is_dir() {
            walk(&p, f)?;
        } else {
            f(&p);
        }
    }
    Ok(())
}

/// Total size in bytes of files under a directory.
pub fn dir_size(dir: &Path) -> Result<u64> {
    let mut total = 0u64;
    walk(dir, &mut |p| {
        total += fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    })?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_core::{AccountId, SourceKind};

    #[test]
    fn write_read_and_orphans() {
        let d = tempfile::tempdir().unwrap();
        let home = Home::new(d.path());
        home.ensure_layout().unwrap();
        let r = SourceRef {
            account_id: AccountId::new("acme").unwrap(),
            source_kind: SourceKind::SlackThread,
            source_id: "C1:1727000000.000100".into(),
            source_url: None,
            created_at: sb_core::util::parse_ts("2026-09-01T10:00:00Z"),
            updated_at: None,
        };
        let dir = entry_dir(&r, Utc::now());
        assert!(dir.starts_with("raw/acme/slack.thread/2026/09/C1_1727000000.000100-"));
        let obj = RawObject {
            role: RawRole::Primary,
            media_type: "application/x-ndjson".into(),
            ext: "jsonl".into(),
            bytes: b"{\"ts\":\"1\"}\n".to_vec(),
        };
        let s = write_object(&home, &dir, 0, &obj).unwrap();
        assert!(s.path.ends_with("/primary.0.jsonl"));
        assert_eq!(dir_of(&s.path), dir);
        let row = RawObjectRow {
            id: 1,
            entry_id: 1,
            role: s.role,
            seq: 0,
            path: s.path.clone(),
            media_type: s.media_type.clone(),
            sha256: s.sha256.clone(),
            size: s.size,
            fetched_at: Utc::now(),
        };
        let segs = read_segments(&home, std::slice::from_ref(&row)).unwrap();
        assert_eq!(segs[0].bytes, obj.bytes);
        assert_eq!(next_seq(&[row], RawRole::Primary), 1);
        assert!(
            find_orphans(&home, std::slice::from_ref(&s.path))
                .unwrap()
                .is_empty()
        );
        assert_eq!(find_orphans(&home, &[]).unwrap().len(), 1);
        delete_paths(&home, &[s.path]);
        assert!(find_orphans(&home, &[]).unwrap().is_empty());
        assert!(home.raw_dir().exists());
    }
}
