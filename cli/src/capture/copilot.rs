//! GitHub Copilot: detected, not read.
//!
//! Two stores exist and neither is verified on a real install: VS Code keeps
//! chat sessions at
//! `<User>/workspaceStorage/<hash>/chatSessions/<sessionId>.json`, and the
//! Copilot CLI is documented as writing `~/.copilot/session-state/<session>/`.
//! The second has no published schema and the third-party descriptions of it
//! disagree with each other.
//!
//! So this adapter says whether Copilot is on the machine and stops there.
//! Guessing at a format would put numbers on a page that nobody can check,
//! which is the one thing madelog must not do.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use super::{Adapter, CapturedSession, Detection, COPILOT_NOTE};

/// Overrides where the Copilot CLI keeps its state, for tests.
pub const HOME_ENV: &str = "MADELOG_COPILOT_HOME";

pub struct Copilot;

impl Adapter for Copilot {
    fn name(&self) -> &str {
        "copilot"
    }

    fn detect(&self) -> Detection {
        match found_at() {
            Some(path) => Detection::found(format!("{COPILOT_NOTE} ({} is there)", path.display())),
            None => Detection::missing("not installed"),
        }
    }

    fn sessions(
        &self,
        _repo_root: &Path,
        _since: DateTime<Utc>,
        _until: DateTime<Utc>,
    ) -> Vec<CapturedSession> {
        Vec::new()
    }

    fn touching(&self, _repo_root: &Path, _since: DateTime<Utc>, _until: DateTime<Utc>) -> usize {
        0
    }
}

/// The first store that exists, if either does.
pub fn found_at() -> Option<PathBuf> {
    if let Some(dir) = cli_state_dir().filter(|d| d.is_dir()) {
        return Some(dir);
    }
    vscode_chat_sessions()
}

fn cli_state_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(HOME_ENV) {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir).join("session-state"));
        }
    }
    Some(dirs::home_dir()?.join(".copilot").join("session-state"))
}

/// The first `chatSessions` directory under VS Code's workspace storage.
fn vscode_chat_sessions() -> Option<PathBuf> {
    let storage = vscode_user_dir()?.join("workspaceStorage");
    let entries = std::fs::read_dir(storage).ok()?;
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path().join("chatSessions"))
        .filter(|p| p.is_dir())
        .collect();
    found.sort();
    found.into_iter().next()
}

fn vscode_user_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("Code")
            .join("User")
    } else if cfg!(windows) {
        dirs::config_dir()?.join("Code").join("User")
    } else {
        home.join(".config").join("Code").join("User")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn copilot_never_returns_a_session() {
        let dir = tempfile::TempDir::new().unwrap();
        let now = Utc::now();
        assert!(Copilot
            .sessions(dir.path(), now - chrono::Duration::days(1), now)
            .is_empty());
        assert_eq!(Copilot.touching(dir.path(), now, now), 0);
        assert_eq!(Copilot.name(), "copilot");
    }

    #[test]
    fn a_state_directory_makes_it_detected_and_still_unsupported() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("session-state")).unwrap();
        std::env::set_var(HOME_ENV, dir.path());
        let detection = Copilot.detect();
        std::env::remove_var(HOME_ENV);

        assert!(detection.installed);
        assert!(
            detection.note.contains("not supported yet"),
            "{detection:?}"
        );
    }
}
