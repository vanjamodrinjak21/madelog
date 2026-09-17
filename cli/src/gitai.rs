//! Reader for Git AI attribution notes.
//!
//! Git AI is installed separately by the user and writes its attribution into
//! a git notes ref. pop never parses editor or tool logs itself; this module
//! is the only place that knows Git AI's format.
//!
//! **Task B implements this.** Until then it returns no sessions, so the
//! post-commit hook records commits normally on a machine without Git AI.
//! See `docs/superpowers/specs/gitai-format.md` and the fixtures that will
//! live in `cli/tests/fixtures/gitai/`.

use anyhow::Result;
use git2::Repository;

use crate::event::AiSessionPayload;

/// AI sessions attributed to `sha`, newest first.
///
/// Returns an empty vector when Git AI is not installed, wrote no note for
/// this commit, or wrote a note this reader cannot parse — recording the
/// commit itself must never depend on it.
///
/// TODO(task B): read the Git AI notes ref via git2 and map it to
/// [`AiSessionPayload`], warning once per process when the note is present
/// but unparseable.
pub fn sessions_for_commit(_repo: &Repository, _sha: &str) -> Result<Vec<AiSessionPayload>> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_placeholder_reader_reports_no_sessions() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        assert!(sessions_for_commit(&repo, &"a".repeat(40))
            .unwrap()
            .is_empty());
    }
}
