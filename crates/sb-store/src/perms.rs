//! Owner-only permissions (ADR-0003): Unix `0700`/`0600`, Windows owner-only ACL.

use std::path::Path;

use crate::error::{Result, StoreError};

/// Restrict a directory to the current user. On Windows the ACL is inherited
/// by files created inside it afterwards.
pub fn make_private_dir(path: &Path) -> Result<()> {
    imp::make_private(path, true)
}

/// Restrict a file to the current user.
pub fn make_private_file(path: &Path) -> Result<()> {
    imp::make_private(path, false)
}

/// Whether a path is accessible only by the current user. `Ok(None)` means the
/// check is not possible on this platform.
pub fn is_private(path: &Path, dir: bool) -> Result<Option<bool>> {
    imp::is_private(path, dir)
}

#[cfg(unix)]
mod imp {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use super::*;

    pub fn make_private(path: &Path, dir: bool) -> Result<()> {
        let mode = if dir { 0o700 } else { 0o600 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|e| StoreError::io(path, e))
    }

    pub fn is_private(path: &Path, _dir: bool) -> Result<Option<bool>> {
        let meta = fs::metadata(path).map_err(|e| StoreError::io(path, e))?;
        Ok(Some(meta.permissions().mode() & 0o077 == 0))
    }
}

#[cfg(windows)]
mod imp {
    //! Uses `icacls`, which ships with every Windows version, to avoid unsafe
    //! Win32 security API code.
    use std::path::Path;
    use std::process::Command;

    use super::*;

    fn current_user() -> String {
        let user = std::env::var("USERNAME").unwrap_or_default();
        match std::env::var("USERDOMAIN") {
            Ok(d) if !d.is_empty() => format!("{d}\\{user}"),
            _ => user,
        }
    }

    pub fn make_private(path: &Path, dir: bool) -> Result<()> {
        let grant = if dir {
            format!("{}:(OI)(CI)F", current_user())
        } else {
            format!("{}:F", current_user())
        };
        let out = Command::new("icacls")
            .arg(path)
            .args(["/inheritance:r", "/grant:r", &grant, "/q"])
            .output()
            .map_err(|e| StoreError::io(path, e))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(StoreError::Invalid(format!(
                "icacls failed on {}: {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }

    pub fn is_private(path: &Path, _dir: bool) -> Result<Option<bool>> {
        let out = Command::new("icacls")
            .arg(path)
            .output()
            .map_err(|e| StoreError::io(path, e))?;
        if !out.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let path_str = path.display().to_string();
        let user = current_user().to_lowercase();
        let mut principals = Vec::new();
        for line in text.lines() {
            let line = line.strip_prefix(&path_str).unwrap_or(line).trim();
            if line.is_empty() || line.starts_with("Successfully") {
                continue;
            }
            if let Some((who, _)) = line.split_once(':') {
                principals.push(who.trim().to_lowercase());
            }
        }
        if principals.is_empty() {
            return Ok(None);
        }
        Ok(Some(principals.iter().all(|p| *p == user)))
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::path::Path;

    use super::*;

    pub fn make_private(_path: &Path, _dir: bool) -> Result<()> {
        Ok(())
    }

    pub fn is_private(_path: &Path, _dir: bool) -> Result<Option<bool>> {
        Ok(None)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn unix_modes() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f");
        std::fs::write(&f, b"x").unwrap();
        make_private_file(&f).unwrap();
        assert_eq!(is_private(&f, false).unwrap(), Some(true));
        make_private_dir(dir.path()).unwrap();
        assert_eq!(is_private(dir.path(), true).unwrap(), Some(true));
    }
}
