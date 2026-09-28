//! `SECOND_BRAIN_HOME` resolution and layout (ADR-0009, architecture.md).

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Result, StoreError};
use crate::perms;

/// Environment variable that overrides the default home.
pub const HOME_ENV: &str = "SECOND_BRAIN_HOME";
/// Catalog database file name.
pub const DB_FILE: &str = "second-brain.db";

/// The resolved home directory and its layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Home { root: root.into() }
    }

    /// Resolve the home: explicit override, then `SECOND_BRAIN_HOME`, then the
    /// OS default.
    pub fn resolve(explicit: Option<&Path>) -> Result<Self> {
        if let Some(p) = explicit {
            return Ok(Home::new(absolute(p)));
        }
        if let Some(p) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            return Ok(Home::new(absolute(Path::new(&p))));
        }
        default_home()
            .map(Home::new)
            .ok_or_else(|| StoreError::Invalid("cannot determine the user data directory".into()))
    }

    /// Whether this home is the OS default.
    pub fn is_default(&self) -> bool {
        default_home().is_some_and(|d| d == self.root)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn db_path(&self) -> PathBuf {
        self.root.join(DB_FILE)
    }
    pub fn raw_dir(&self) -> PathBuf {
        self.root.join("raw")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }
    pub fn locks_dir(&self) -> PathBuf {
        self.root.join("locks")
    }
    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// Resolve a stored relative path (always `/`-separated) under the home.
    pub fn resolve_rel(&self, rel: &str) -> PathBuf {
        let mut p = self.root.clone();
        for part in rel.split('/').filter(|s| !s.is_empty()) {
            p.push(part);
        }
        p
    }

    /// Whether the home and its database exist.
    pub fn is_initialized(&self) -> bool {
        self.db_path().is_file()
    }

    /// Create the directory layout with private permissions. Idempotent.
    pub fn ensure_layout(&self) -> Result<()> {
        fs::create_dir_all(&self.root).map_err(|e| StoreError::io(&self.root, e))?;
        perms::make_private_dir(&self.root)?;
        for d in [
            self.raw_dir(),
            self.logs_dir(),
            self.locks_dir(),
            self.tmp_dir(),
        ] {
            fs::create_dir_all(&d).map_err(|e| StoreError::io(&d, e))?;
        }
        Ok(())
    }
}

/// The OS default home: `%LOCALAPPDATA%\second-brain`,
/// `~/Library/Application Support/second-brain`, or `~/.local/share/second-brain`.
pub fn default_home() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.data_local_dir().join("second-brain"))
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_rel_splits_on_slash() {
        let h = Home::new("/x");
        assert_eq!(
            h.resolve_rel("raw/a/b.md"),
            Path::new("/x").join("raw").join("a").join("b.md")
        );
    }

    #[test]
    fn explicit_home_wins() {
        let h = Home::resolve(Some(Path::new("/tmp/sbhome"))).unwrap();
        assert!(h.root().ends_with("sbhome"));
    }
}
