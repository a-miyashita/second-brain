//! `sb setup env`: persist `SECOND_BRAIN_HOME` when a non-default home is used.

use std::path::{Path, PathBuf};

use crate::error::{Result, SetupError, run};

const BEGIN: &str = "# BEGIN second-brain";
const END: &str = "# END second-brain";

/// The shell rc file for `$SHELL`, and the line to put in it.
pub fn shell_rc(user_home: &Path, shell: &str, home: &Path) -> (PathBuf, String) {
    let quoted = format!("'{}'", home.display().to_string().replace('\'', "'\\''"));
    let name = Path::new(shell)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    match name {
        "fish" => (
            user_home.join(".config").join("fish").join("config.fish"),
            format!("set -gx SECOND_BRAIN_HOME {quoted}"),
        ),
        "zsh" => (
            user_home.join(".zshrc"),
            format!("export SECOND_BRAIN_HOME={quoted}"),
        ),
        "bash" if cfg!(target_os = "macos") => (
            user_home.join(".bash_profile"),
            format!("export SECOND_BRAIN_HOME={quoted}"),
        ),
        _ => (
            user_home.join(".bashrc"),
            format!("export SECOND_BRAIN_HOME={quoted}"),
        ),
    }
}

/// Replace (or add) the marked block in an rc file's content.
pub fn upsert_block(existing: &str, line: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for l in existing.lines() {
        if l.trim() == BEGIN {
            inside = true;
            continue;
        }
        if l.trim() == END {
            inside = false;
            continue;
        }
        if !inside {
            out.push_str(l);
            out.push('\n');
        }
    }
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
    out.push_str(&format!("{BEGIN}\n{line}\n{END}\n"));
    out
}

/// Append (or update) the export line in an rc file.
pub fn write_rc(rc: &Path, line: &str) -> Result<()> {
    let existing = std::fs::read_to_string(rc).unwrap_or_default();
    if let Some(parent) = rc.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SetupError::io(parent, e))?;
    }
    std::fs::write(rc, upsert_block(&existing, line)).map_err(|e| SetupError::io(rc, e))
}

/// Windows: write `HKCU\Environment\SECOND_BRAIN_HOME` and broadcast the
/// change (`setx` does both).
pub fn set_windows_user_env(home: &Path) -> Result<()> {
    run(
        "setx",
        &["SECOND_BRAIN_HOME", &home.display().to_string()],
        None,
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc_detection_and_block() {
        let (rc, line) = shell_rc(Path::new("/u"), "/bin/zsh", Path::new("/data/sb"));
        assert_eq!(rc, Path::new("/u/.zshrc"));
        assert_eq!(line, "export SECOND_BRAIN_HOME='/data/sb'");
        let (rc, line) = shell_rc(Path::new("/u"), "/usr/bin/fish", Path::new("/data/sb"));
        assert!(rc.ends_with("config.fish"));
        assert!(line.starts_with("set -gx"));
        let once = upsert_block("alias ll='ls -l'\n", &line);
        let twice = upsert_block(&once, &line);
        assert_eq!(once, twice);
        assert!(once.starts_with("alias ll='ls -l'\n\n# BEGIN second-brain\n"));
    }
}
