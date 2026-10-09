//! Agent skill files, embedded in the binary (ADR-0010, agent-integration.md).

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Result, SetupError};

/// Embedded skill files: (relative path, content).
pub const FILES: &[(&str, &str)] = &[
    (
        "SKILL.md",
        include_str!("../assets/skills/second-brain/SKILL.md"),
    ),
    (
        "references/search-guide.md",
        include_str!("../assets/skills/second-brain/references/search-guide.md"),
    ),
    (
        "references/ingest-guide.md",
        include_str!("../assets/skills/second-brain/references/ingest-guide.md"),
    ),
];

/// An agent that can load the skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Copilot,
    Claude,
    Codex,
}

impl Target {
    pub const ALL: [Target; 3] = [Target::Copilot, Target::Claude, Target::Codex];

    pub fn name(self) -> &'static str {
        match self {
            Target::Copilot => "copilot",
            Target::Claude => "claude",
            Target::Codex => "codex",
        }
    }

    pub fn parse(s: &str) -> Option<Vec<Target>> {
        match s {
            "copilot" => Some(vec![Target::Copilot]),
            "claude" => Some(vec![Target::Claude]),
            "codex" => Some(vec![Target::Codex]),
            "all" => Some(Target::ALL.to_vec()),
            _ => None,
        }
    }

    /// The agent's CLI binary, used to detect whether it is installed.
    pub fn binary(self) -> &'static str {
        match self {
            Target::Copilot => "copilot",
            Target::Claude => "claude",
            Target::Codex => "codex",
        }
    }

    /// Skill directory under the user's home.
    pub fn dir(self, user_home: &Path) -> PathBuf {
        let base = match self {
            Target::Copilot => ".copilot",
            Target::Claude => ".claude",
            Target::Codex => ".codex",
        };
        user_home.join(base).join("skills").join("second-brain")
    }

    pub fn is_installed_agent(self) -> bool {
        which::which(self.binary()).is_ok()
    }
}

/// The user's home directory.
pub fn user_home() -> Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .ok_or(SetupError::NoUserHome)
}

/// Read the `version:` field of a SKILL.md frontmatter.
pub fn parse_version(skill_md: &str) -> Option<String> {
    let mut lines = skill_md.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for l in lines {
        if l.trim() == "---" {
            return None;
        }
        if let Some(v) = l.strip_prefix("version:") {
            return Some(v.trim().trim_matches('"').to_string());
        }
    }
    None
}

/// The version embedded in this binary.
pub fn embedded_version() -> String {
    parse_version(FILES[0].1).unwrap_or_default()
}

/// Install the skill for a target. Returns the directory.
pub fn install(target: Target, user_home: &Path) -> Result<PathBuf> {
    let dir = target.dir(user_home);
    for (rel, content) in FILES {
        let p = rel.split('/').fold(dir.clone(), |acc, part| acc.join(part));
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).map_err(|e| SetupError::io(parent, e))?;
        }
        fs::write(&p, content).map_err(|e| SetupError::io(&p, e))?;
    }
    Ok(dir)
}

/// Remove the skill of a target. Returns whether it existed.
pub fn remove(target: Target, user_home: &Path) -> Result<bool> {
    let dir = target.dir(user_home);
    if !dir.exists() {
        return Ok(false);
    }
    fs::remove_dir_all(&dir).map_err(|e| SetupError::io(&dir, e))?;
    Ok(true)
}

/// Installed version of a target's skill, if installed.
pub fn installed_version(target: Target, user_home: &Path) -> Option<String> {
    let p = target.dir(user_home).join("SKILL.md");
    fs::read_to_string(p).ok().and_then(|s| parse_version(&s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_skill_has_version_and_install_round_trips() {
        assert!(!embedded_version().is_empty());
        let d = tempfile::tempdir().unwrap();
        let dir = install(Target::Claude, d.path()).unwrap();
        assert!(dir.join("references").join("search-guide.md").is_file());
        assert_eq!(
            installed_version(Target::Claude, d.path()),
            Some(embedded_version())
        );
        assert_eq!(installed_version(Target::Copilot, d.path()), None);
        assert!(remove(Target::Claude, d.path()).unwrap());
        assert!(!remove(Target::Claude, d.path()).unwrap());
        assert_eq!(parse_version("no frontmatter"), None);
    }
}
