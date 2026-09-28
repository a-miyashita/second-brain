//! The sync lock (ADR-0009): one sync process at a time.
//!
//! An OS advisory lock on `locks/sync.lock` is used. The OS releases it when the
//! process dies, so a crashed run never leaves a stale lock behind.

use std::fs::{File, OpenOptions};
use std::io::Write;

use fs4::fs_std::FileExt;

use crate::error::{Result, StoreError};
use crate::home::Home;

/// A held lock; released on drop.
#[derive(Debug)]
pub struct SyncLock {
    file: File,
}

impl SyncLock {
    /// Try to take the lock without waiting.
    pub fn try_acquire(home: &Home, holder: &str) -> Result<Self> {
        let dir = home.locks_dir();
        std::fs::create_dir_all(&dir).map_err(|e| StoreError::io(&dir, e))?;
        let path = dir.join("sync.lock");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| StoreError::io(&path, e))?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(true) => {}
            Ok(false) => return Err(StoreError::Locked(path)),
            Err(e) => return Err(StoreError::io(&path, e)),
        }
        let _ = file.set_len(0);
        let _ = writeln!(file, "pid={} holder={holder}", std::process::id());
        Ok(SyncLock { file })
    }
}

impl Drop for SyncLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_fails_until_release() {
        let d = tempfile::tempdir().unwrap();
        let home = Home::new(d.path());
        let l = SyncLock::try_acquire(&home, "a").unwrap();
        assert!(matches!(
            SyncLock::try_acquire(&home, "b"),
            Err(StoreError::Locked(_))
        ));
        drop(l);
        SyncLock::try_acquire(&home, "c").unwrap();
    }
}
