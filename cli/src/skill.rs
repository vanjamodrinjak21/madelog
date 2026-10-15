//! The Claude Code skill madelog ships with.
//!
//! With it installed, Claude records decisions on its own the way the git hook
//! records commits, and knows which commands wait for the person (publish,
//! unpublish, pro, init). It lives at `~/.claude/skills/madelog/SKILL.md`,
//! which Claude Code loads in every session.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The skill, compiled into the binary so it always matches this version.
pub const SKILL: &str = include_str!("../assets/claude-skill.md");

/// What installing did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// Claude Code is not on this machine; nothing was written.
    NoClaude,
    /// The file was already this version.
    Current(PathBuf),
    /// Written for the first time, or brought up to this version.
    Written(PathBuf),
}

/// `~/.claude`, or `MADELOG_CLAUDE_HOME` when set.
pub fn claude_home() -> Option<PathBuf> {
    crate::capture::home_override(crate::capture::claude::HOME_ENV, ".claude")
}

/// Install or update the skill when Claude Code is present. Never creates
/// `~/.claude` itself: no Claude Code, no skill.
pub fn install() -> Result<Installed> {
    match claude_home() {
        Some(home) if home.is_dir() => install_into(&home),
        _ => Ok(Installed::NoClaude),
    }
}

pub fn install_into(claude_home: &Path) -> Result<Installed> {
    let path = claude_home.join("skills").join("madelog").join("SKILL.md");
    if std::fs::read_to_string(&path).ok().as_deref() == Some(SKILL) {
        return Ok(Installed::Current(path));
    }
    let dir = path.parent().expect("the skill path has a parent");
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(&path, SKILL).with_context(|| format!("writing {}", path.display()))?;
    Ok(Installed::Written(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skill_has_frontmatter_claude_code_can_read() {
        assert!(SKILL.starts_with("---\nname: madelog\ndescription: "));
        assert!(SKILL.contains("madelog note"));
        assert!(SKILL.contains("Never publish"));
    }

    #[test]
    fn install_writes_once_and_then_reports_current() {
        let dir = tempfile::TempDir::new().unwrap();
        let first = install_into(dir.path()).unwrap();
        let path = dir.path().join("skills/madelog/SKILL.md");
        assert_eq!(first, Installed::Written(path.clone()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SKILL);
        assert_eq!(
            install_into(dir.path()).unwrap(),
            Installed::Current(path.clone())
        );

        // An older copy is brought up to date.
        std::fs::write(&path, "old").unwrap();
        assert_eq!(
            install_into(dir.path()).unwrap(),
            Installed::Written(path.clone())
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SKILL);
    }
}
