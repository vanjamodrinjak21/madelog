//! Matching what an AI tool wrote against what a commit actually contains.
//!
//! This is the only place a number like "41 of 80 lines came from the model"
//! is produced, and it is produced the only way that can be checked: by
//! comparing bytes.
//!
//! For one file and one session:
//!
//! - `ai_lines` — lines the commit added that the session wrote verbatim.
//!   Counted with multiplicity, capped by how often each side has the line, so
//!   a session that wrote `}` once cannot claim the twelve the commit added.
//! - `ai_lines_modified_by_human` — lines the session wrote that the committed
//!   file does not contain. The model proposed them and the person changed
//!   them before committing.
//! - `accepted` — whether anything the session wrote survived into the commit.
//!
//! Blank and whitespace-only lines never count for anyone: they are identical
//! in every file ever written, and attributing them would inflate every
//! number on the page.

use std::collections::BTreeMap;
use std::path::Path;

use crate::event::{AiSessionFile, AiSessionPayload};

use super::{is_meaningful, relative, CapturedSession};

/// One file as a commit left it: the lines the commit added, and the lines
/// the file ended up with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitFileDiff {
    /// Repository-relative, forward slashes — the same spelling the commit
    /// event uses.
    pub path: String,
    /// The `+` lines of the commit's hunks, without the marker.
    pub added: Vec<String>,
    /// The committed file's contents, line by line. Empty for a file the
    /// commit deleted.
    pub content: Vec<String>,
}

/// One commit, as attribution needs to see it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitDiff {
    pub files: Vec<CommitFileDiff>,
}

impl CommitDiff {
    pub fn file(&self, path: &str) -> Option<&CommitFileDiff> {
        self.files.iter().find(|f| f.path == path)
    }
}

/// Turn captured sessions into the events the log stores.
///
/// A session is kept when it put at least one line into this commit, and also
/// when it wrote no files at all — a conversation, or a tool like Cursor that
/// keeps no verbatim text. What is dropped is a session that wrote files and
/// landed none of them here: that session belongs to the commit that does
/// carry its work, and it is recorded there rather than against every commit
/// made the same day.
///
/// `extra` is any other source of attribution for the same commit — today the
/// Git AI notes, when a user happens to run it. Sources are merged per file,
/// never added up, and a source madelog has no session for is carried through
/// as its own event rather than dropped.
pub fn attribute(
    sessions: &[CapturedSession],
    commit: &CommitDiff,
    repo_root: &Path,
    extra: &[AiSessionPayload],
) -> Vec<AiSessionPayload> {
    let mut out: Vec<AiSessionPayload> = sessions
        .iter()
        .map(|session| (session, one(session, commit, repo_root)))
        .filter(|(session, payload)| !payload.files.is_empty() || session.writes.is_empty())
        .map(|(_, payload)| payload)
        .collect();

    for other in extra {
        match out.iter_mut().find(|p| same_session(p, other)) {
            Some(existing) => merge_files(existing, other),
            None => out.push(other.clone()),
        }
    }
    out
}

/// Two records describe one session when the tool and the id agree.
fn same_session(a: &AiSessionPayload, b: &AiSessionPayload) -> bool {
    a.session_id == b.session_id && a.tool == b.tool
}

/// Merge `other`'s files into `into`, taking the larger count per path. Two
/// sources looking at the same commit see the same lines; adding them up
/// would count every line twice.
fn merge_files(into: &mut AiSessionPayload, other: &AiSessionPayload) {
    for file in &other.files {
        match into.files.iter_mut().find(|f| f.path == file.path) {
            Some(existing) => {
                existing.ai_lines = existing.ai_lines.max(file.ai_lines);
                existing.ai_lines_modified_by_human = existing
                    .ai_lines_modified_by_human
                    .max(file.ai_lines_modified_by_human);
            }
            None => into.files.push(file.clone()),
        }
    }
    into.files.sort_by(|a, b| a.path.cmp(&b.path));
    into.accepted = into.accepted || other.accepted;
    if into.model.is_none() {
        into.model = other.model.clone();
    }
    if into.prompt.is_none() {
        into.prompt = other.prompt.clone();
    }
}

/// One session against one commit.
fn one(session: &CapturedSession, commit: &CommitDiff, repo_root: &Path) -> AiSessionPayload {
    let mut files: Vec<AiSessionFile> = Vec::new();

    for write in &session.writes {
        let Some(path) = relative(repo_root, &write.path) else {
            continue;
        };
        let Some(committed) = commit.file(&path) else {
            // The session wrote this file, but this commit does not carry it.
            // Another commit will, and that is where it gets attributed.
            continue;
        };
        let written = multiset(&write.lines);
        let ai_lines = overlap(&written, &multiset(&committed.added));
        let modified = missing_from(&written, &multiset(&committed.content));
        if ai_lines == 0 && modified == 0 {
            continue;
        }
        files.push(AiSessionFile {
            path,
            ai_lines,
            ai_lines_modified_by_human: modified,
        });
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    let accepted = files.iter().any(|f| f.ai_lines > 0);

    AiSessionPayload {
        tool: session.tool.clone(),
        model: session.model.clone(),
        session_id: session.session_id.clone(),
        prompt: session.prompt.clone(),
        // madelog never stores the model's reply. It is the one part of a
        // session with no bearing on what the commit contains.
        response_excerpt: None,
        files,
        accepted,
    }
}

/// How often each meaningful line appears.
fn multiset(lines: &[String]) -> BTreeMap<&str, u64> {
    let mut out: BTreeMap<&str, u64> = BTreeMap::new();
    for line in lines.iter().filter(|l| is_meaningful(l)) {
        *out.entry(line.as_str()).or_default() += 1;
    }
    out
}

/// Lines in both, counted as often as the thinner side has them.
fn overlap(a: &BTreeMap<&str, u64>, b: &BTreeMap<&str, u64>) -> u64 {
    a.iter()
        .map(|(line, n)| (*n).min(b.get(line).copied().unwrap_or(0)))
        .sum()
}

/// Lines in `a` that `b` does not have, counted with multiplicity.
fn missing_from(a: &BTreeMap<&str, u64>, b: &BTreeMap<&str, u64>) -> u64 {
    a.iter()
        .map(|(line, n)| n.saturating_sub(b.get(line).copied().unwrap_or(0)))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{Write, WriteKind};
    use chrono::Utc;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    fn session(root: &Path, file: &str, written: &str) -> CapturedSession {
        let now = Utc::now();
        CapturedSession {
            tool: "claude-code".to_string(),
            session_id: "s1".to_string(),
            model: Some("claude-sonnet-4-5".to_string()),
            started: now,
            ended: now,
            prompt: Some("write the parser".to_string()),
            prompt_count: 1,
            title: None,
            writes: vec![Write {
                path: root.join(file),
                lines: lines(written),
                kind: WriteKind::Create,
            }],
            lines_added_hint: None,
        }
    }

    fn commit(path: &str, added: &str, content: &str) -> CommitDiff {
        CommitDiff {
            files: vec![CommitFileDiff {
                path: path.to_string(),
                added: lines(added),
                content: lines(content),
            }],
        }
    }

    #[test]
    fn lines_the_commit_added_verbatim_are_the_ai_lines() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let written = "fn parse() {\n    let a = 1;\n    let b = 2;\n}";
        let s = session(root, "src/p.rs", written);
        let c = commit("src/p.rs", written, written);

        let out = attribute(&[s], &c, root, &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].files.len(), 1);
        assert_eq!(out[0].files[0].path, "src/p.rs");
        assert_eq!(out[0].files[0].ai_lines, 4);
        assert_eq!(out[0].files[0].ai_lines_modified_by_human, 0);
        assert!(out[0].accepted);
        assert_eq!(out[0].tool, "claude-code");
        assert_eq!(out[0].response_excerpt, None);
    }

    #[test]
    fn lines_the_human_changed_before_committing_are_counted_apart() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let written = "fn parse() {\n    let a = 1;\n    let b = 2;\n}";
        // The person kept the shape and rewrote one line.
        let committed = "fn parse() {\n    let a = 1;\n    let b = 99;\n}";
        let s = session(root, "src/p.rs", written);
        let c = commit("src/p.rs", committed, committed);

        let out = attribute(&[s], &c, root, &[]);
        let f = &out[0].files[0];
        assert_eq!(f.ai_lines, 3, "three lines survived word for word");
        assert_eq!(f.ai_lines_modified_by_human, 1, "`let b = 2;` was changed");
        assert!(out[0].accepted);
    }

    #[test]
    fn a_session_whose_work_did_not_survive_is_not_accepted() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let s = session(root, "src/p.rs", "let a = 1;\nlet b = 2;");
        let c = commit("src/p.rs", "let c = 3;", "let c = 3;");

        let out = attribute(&[s], &c, root, &[]);
        assert_eq!(out[0].files[0].ai_lines, 0);
        assert_eq!(out[0].files[0].ai_lines_modified_by_human, 2);
        assert!(!out[0].accepted);
    }

    #[test]
    fn blank_lines_belong_to_nobody() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let s = session(root, "a.rs", "\n   \nlet a = 1;\n\n");
        let c = commit("a.rs", "\n\n\nlet a = 1;\n\n\n", "let a = 1;");

        let out = attribute(&[s], &c, root, &[]);
        assert_eq!(out[0].files[0].ai_lines, 1);
        assert_eq!(out[0].files[0].ai_lines_modified_by_human, 0);
    }

    #[test]
    fn a_line_cannot_be_claimed_more_often_than_it_was_written() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        // The session wrote one closing brace; the commit added twelve.
        let s = session(root, "a.rs", "}");
        let c = commit("a.rs", "}\n}\n}\n}\n}\n}\n}\n}\n}\n}\n}\n}", "}");

        let out = attribute(&[s], &c, root, &[]);
        assert_eq!(out[0].files[0].ai_lines, 1);
    }

    #[test]
    fn a_file_this_commit_does_not_carry_is_left_for_the_commit_that_does() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let s = session(root, "other.rs", "let a = 1;");
        let c = commit("a.rs", "let a = 1;", "let a = 1;");

        assert!(
            attribute(&[s], &c, root, &[]).is_empty(),
            "the session wrote other.rs; the commit that carries other.rs records it"
        );
    }

    #[test]
    fn a_write_outside_the_repository_never_reaches_a_payload() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let mut s = session(root, "a.rs", "let a = 1;");
        s.writes[0].path = std::path::PathBuf::from("/etc/hosts");
        let c = commit("a.rs", "let a = 1;", "let a = 1;");

        assert!(attribute(&[s], &c, root, &[]).is_empty());
    }

    #[test]
    fn another_source_is_merged_by_file_and_never_added_up() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let s = session(root, "a.rs", "let a = 1;\nlet b = 2;");
        let c = commit("a.rs", "let a = 1;\nlet b = 2;", "let a = 1;\nlet b = 2;");

        let from_notes = AiSessionPayload {
            tool: "claude-code".into(),
            model: None,
            session_id: "s1".into(),
            prompt: None,
            response_excerpt: None,
            files: vec![
                AiSessionFile {
                    path: "a.rs".into(),
                    ai_lines: 1,
                    ai_lines_modified_by_human: 0,
                },
                AiSessionFile {
                    path: "b.rs".into(),
                    ai_lines: 5,
                    ai_lines_modified_by_human: 1,
                },
            ],
            accepted: true,
        };

        let out = attribute(&[s], &c, root, &[from_notes]);
        assert_eq!(out.len(), 1, "one session, two sources");
        assert_eq!(out[0].files.len(), 2);
        assert_eq!(out[0].files[0].path, "a.rs");
        assert_eq!(out[0].files[0].ai_lines, 2, "the fuller count wins, not 3");
        assert_eq!(out[0].files[1].path, "b.rs");
        assert_eq!(out[0].files[1].ai_lines, 5, "a file only notes saw");
    }

    #[test]
    fn a_source_with_no_session_of_ours_is_carried_through() {
        let dir = tempfile::TempDir::new().unwrap();
        let other = AiSessionPayload {
            tool: "amp".into(),
            model: None,
            session_id: "s_ff00".into(),
            prompt: None,
            response_excerpt: None,
            files: vec![],
            accepted: true,
        };
        let out = attribute(
            &[],
            &CommitDiff::default(),
            dir.path(),
            std::slice::from_ref(&other),
        );
        assert_eq!(out, vec![other]);
    }

    #[test]
    fn a_cursor_session_records_that_it_happened_and_claims_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let now = Utc::now();
        let s = CapturedSession {
            tool: "cursor".into(),
            session_id: "cmp-1".into(),
            model: Some("claude-4.5".into()),
            started: now,
            ended: now,
            prompt: None,
            prompt_count: 0,
            title: None,
            writes: vec![],
            lines_added_hint: Some(40),
        };
        let out = attribute(&[s], &CommitDiff::default(), dir.path(), &[]);
        assert_eq!(out[0].tool, "cursor");
        assert!(out[0].files.is_empty());
        assert!(!out[0].accepted);
        assert_eq!(out[0].prompt, None);
    }
}
