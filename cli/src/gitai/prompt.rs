//! Best-effort prompt lookup for Claude Code sessions.
//!
//! Git AI notes carry no prompt text — by design. For `tool == "claude"` the
//! agent id is the Claude Code session UUID, and Claude Code keeps that
//! session's transcript at
//! `~/.claude/projects/<encoded working directory>/<session id>.jsonl`.
//!
//! Everything here is best effort: a missing file, an unreadable line, a
//! renamed directory, a prompt matching `privacy.redact` — all of them mean
//! "no prompt", never an error.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::gitrepo::ExcerptFilter;

/// Overrides `~/.claude/projects`, so tests never read a real transcript.
pub const PROJECTS_DIR_ENV: &str = "POP_CLAUDE_PROJECTS_DIR";

/// The page shows a prompt, not a transcript.
pub const MAX_PROMPT_CHARS: usize = 500;

/// Where Claude Code keeps its transcripts, or `None` without a home
/// directory.
pub fn projects_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(PROJECTS_DIR_ENV) {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    Some(dirs::home_dir()?.join(".claude").join("projects"))
}

/// Claude Code's name for a working directory.
///
/// Verified against a real `~/.claude/projects` (2026-09-17): every character
/// that is not an ASCII letter or digit becomes `-`, so `/` and spaces and
/// dots all collapse the same way. A trailing separator is dropped first,
/// because git2 hands back working directories with one.
pub fn encode_project_dir(path: &Path) -> String {
    encode(&trimmed(path))
}

fn encode(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The pre-2025 rule, kept as a fallback: only `/` becomes `-`.
fn encode_slashes_only(path: &str) -> String {
    path.replace(['/', '\\'], "-")
}

fn trimmed(path: &Path) -> String {
    let s = path.to_string_lossy().into_owned();
    let t = s.trim_end_matches(['/', '\\']);
    if t.is_empty() {
        s
    } else {
        t.to_string()
    }
}

/// The transcript of `session_id` for a repository at `workdir`, if one is
/// where we expect it.
pub fn transcript_path(base: &Path, workdir: &Path, session_id: &str) -> Option<PathBuf> {
    if !is_safe_session_id(session_id) {
        return None;
    }
    let file = format!("{session_id}.jsonl");
    for dir in candidate_dirs(workdir) {
        let path = base.join(dir).join(&file);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// A note is data from another tool: never let a key out of `base`.
fn is_safe_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !session_id.starts_with('.')
}

/// Directory names worth trying, most likely first.
fn candidate_dirs(workdir: &Path) -> Vec<String> {
    let mut paths = vec![trimmed(workdir)];
    if let Ok(real) = workdir.canonicalize() {
        let real = trimmed(&real);
        if !paths.contains(&real) {
            paths.push(real);
        }
    }
    let mut out = Vec::with_capacity(paths.len() * 2);
    for path in &paths {
        for encoded in [encode(path), encode_slashes_only(path)] {
            if !out.contains(&encoded) {
                out.push(encoded);
            }
        }
    }
    out
}

/// The first thing you asked in that session: collapsed to one line, cut to
/// [`MAX_PROMPT_CHARS`], and dropped whole if `filter` rejects it.
pub fn lookup(
    base: &Path,
    workdir: &Path,
    session_id: &str,
    filter: &ExcerptFilter,
) -> Option<String> {
    let path = transcript_path(base, workdir, session_id)?;
    let body = std::fs::read_to_string(path).ok()?;
    let text = body.lines().find_map(first_user_text)?;
    let prompt = shorten(&text)?;
    // `privacy.redact` is all or nothing here: a masked prompt still leaks
    // its shape, and we would rather show nothing.
    filter.allows(&prompt).then_some(prompt)
}

/// The user's own words in one transcript line, if that is what it is.
fn first_user_text(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type")?.as_str()? != "user" {
        return None;
    }
    // Claude Code injects its own `user` entries (command output, tool
    // results, reminders). None of those are a prompt.
    if value.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let text = match value.get("message")?.get("content")? {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            let parts: Vec<&str> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect();
            parts.join(" ")
        }
        _ => return None,
    };
    (!text.trim().is_empty()).then_some(text)
}

/// One line, at most [`MAX_PROMPT_CHARS`] characters.
fn shorten(text: &str) -> Option<String> {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.chars().take(MAX_PROMPT_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_matches_the_directories_on_disk() {
        assert_eq!(
            encode_project_dir(Path::new("/Users/v/Desktop/CODING PROJECTS/POP CLI")),
            "-Users-v-Desktop-CODING-PROJECTS-POP-CLI"
        );
        assert_eq!(
            encode_project_dir(Path::new("/private/tmp")),
            "-private-tmp"
        );
    }

    #[test]
    fn a_trailing_separator_is_dropped_before_encoding() {
        assert_eq!(
            encode_project_dir(Path::new("/a/b/")),
            encode_project_dir(Path::new("/a/b"))
        );
        // A root working directory is still something.
        assert_eq!(encode_project_dir(Path::new("/")), "-");
    }

    #[test]
    fn session_ids_that_could_escape_the_directory_are_refused() {
        assert!(is_safe_session_id("aa2b7669-72a3-4ce8-8fbf-62fd9caee4f5"));
        assert!(is_safe_session_id("cur_7781.2"));
        assert!(!is_safe_session_id("../../etc/passwd"));
        assert!(!is_safe_session_id("a/b"));
        assert!(!is_safe_session_id(".hidden"));
        assert!(!is_safe_session_id(""));
    }

    #[test]
    fn only_user_entries_with_words_count() {
        assert_eq!(
            first_user_text(r#"{"type":"user","message":{"content":"hi"}}"#).as_deref(),
            Some("hi")
        );
        assert_eq!(first_user_text(r#"{"type":"assistant"}"#), None);
        assert_eq!(
            first_user_text(r#"{"type":"user","message":{"content":"  "}}"#),
            None
        );
        assert_eq!(
            first_user_text(r#"{"type":"user","isMeta":true,"message":{"content":"x"}}"#),
            None
        );
        assert_eq!(first_user_text("}{"), None);
    }

    #[test]
    fn shortening_collapses_whitespace_and_cuts() {
        assert_eq!(shorten(" a\n  b \t c ").as_deref(), Some("a b c"));
        assert_eq!(shorten("   "), None);
        let long = "x".repeat(900);
        assert_eq!(shorten(&long).unwrap().len(), MAX_PROMPT_CHARS);
    }

    #[test]
    fn the_slash_only_encoding_is_tried_as_well() {
        let dirs = candidate_dirs(Path::new("/a/b c"));
        assert!(dirs.contains(&"-a-b-c".to_string()));
        assert!(dirs.contains(&"-a-b c".to_string()));
    }
}
