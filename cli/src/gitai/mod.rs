//! Reader for Git AI attribution notes.
//!
//! Git AI is installed separately by the user and writes its attribution into
//! `refs/notes/ai`, one note per commit. madelog never parses editor or tool logs
//! itself; this module is the only place that knows Git AI's format.
//!
//! What a note can and cannot tell us is worked out in
//! `docs/superpowers/specs/gitai-format.md`. The short version: the
//! attestation is final authorship at that commit, so a line attributed to a
//! session is AI-written and accepted unchanged, and human rework of AI lines
//! shows up later as a pattern, not here.

pub mod note;
pub mod prompt;

use std::path::Path;
use std::sync::{Once, OnceLock};

use anyhow::{Context, Result};
use git2::{Oid, Repository};

use crate::config::Config;
use crate::event::{AiSessionFile, AiSessionPayload};
use crate::gitrepo::ExcerptFilter;

pub use note::{parse as parse_note, Note, Route};

/// The only ref madelog reads. `refs/notes/commits`, `refs/notes/ai-remote/*` and
/// `refs/notes/ai-display` are none of our business.
pub const NOTES_REF: &str = "refs/notes/ai";

/// AI sessions attributed to `sha`, in the order they first appear in the
/// note.
///
/// Returns an empty vector when Git AI is not installed, wrote no note for
/// this commit, or wrote a note this reader cannot parse — recording the
/// commit itself must never depend on it. Both of the last two cases warn
/// once per process.
pub fn sessions_for_commit(repo: &Repository, sha: &str) -> Result<Vec<AiSessionPayload>> {
    let oid = Oid::from_str(sha).with_context(|| format!("`{sha}` is not a commit id"))?;

    let note = match repo.find_note(Some(NOTES_REF), oid) {
        Ok(note) => note,
        // No ref, or no note on this commit. Both are ordinary.
        Err(e) if e.code() == git2::ErrorCode::NotFound => {
            warn_git_ai_missing();
            return Ok(Vec::new());
        }
        Err(e) => {
            return Err(anyhow::Error::new(e).context(format!("reading {NOTES_REF} for {sha}")))
        }
    };

    let Some(blob) = note.message() else {
        warn_unreadable("the note is not valid UTF-8");
        return Ok(Vec::new());
    };

    let parsed = match note::parse(blob) {
        Ok(parsed) => parsed,
        Err(err) => {
            warn_unreadable(&format!("{err:#}"));
            return Ok(Vec::new());
        }
    };

    Ok(to_sessions(&parsed, repo.workdir()))
}

/// Map a parsed note to events. Split out so it is testable without a
/// repository on disk.
pub fn to_sessions(note: &Note, workdir: Option<&Path>) -> Vec<AiSessionPayload> {
    let filter = redaction_filter(workdir);
    let mut out = Vec::new();

    for attribution in note.attribution() {
        // `overriden_lines` only exists on the legacy prompt records; a
        // single v3 note cannot show human rework at all.
        let (agent, overridden) = match attribution.route {
            Route::Human => continue,
            Route::Session => match note.sessions.get(&attribution.key) {
                Some(record) => (record.agent_id.as_ref(), 0),
                None => continue, // unknown key: counted, not invented
            },
            Route::Prompt => match note.prompts.get(&attribution.key) {
                Some(record) => (record.agent_id.as_ref(), record.overriden_lines),
                None => continue,
            },
        };

        let tool = map_tool(agent.and_then(|a| a.tool.as_deref()).unwrap_or("other"));
        let model = agent.and_then(|a| a.model.clone());
        let session_id = agent
            .and_then(|a| a.id.clone())
            .unwrap_or_else(|| attribution.key.clone());

        let counts: Vec<u64> = attribution.files.iter().map(|f| f.lines).collect();
        let files = attribution
            .files
            .iter()
            .zip(spread_proportionally(overridden, &counts))
            .map(|(file, modified)| AiSessionFile {
                path: file.path.clone(),
                ai_lines: file.lines,
                ai_lines_modified_by_human: modified,
            })
            .collect();

        let prompt = if tool == "claude-code" {
            workdir.and_then(|w| claude_prompt(w, &session_id, &filter))
        } else {
            None
        };

        out.push(AiSessionPayload {
            tool,
            model,
            session_id,
            prompt,
            // Git AI never stores the model's reply, and we do not go looking.
            response_excerpt: None,
            files,
            // A key with attributed lines wrote lines that survived the commit.
            accepted: true,
        });
    }

    out
}

/// Git AI's tool names, in ours.
///
/// Anything we have no name for keeps Git AI's, so a tool added upstream
/// shows up on the page instead of disappearing into "other".
pub fn map_tool(tool: &str) -> String {
    match tool {
        "claude" => "claude-code".to_string(),
        other if other.starts_with("github-copilot") => "copilot".to_string(),
        other => other.to_string(),
    }
}

/// Split `total` over `counts` in proportion, giving no file more than its own
/// line count. Largest remainder first, so the parts add up exactly.
pub fn spread_proportionally(total: u64, counts: &[u64]) -> Vec<u64> {
    let sum: u64 = counts.iter().sum();
    if total == 0 || sum == 0 {
        return vec![0; counts.len()];
    }
    if total >= sum {
        return counts.to_vec();
    }

    let mut parts: Vec<u64> = counts.iter().map(|n| total * n / sum).collect();
    let mut left = total - parts.iter().sum::<u64>();

    let mut order: Vec<usize> = (0..counts.len()).collect();
    order.sort_by(|a, b| {
        let ra = (total * counts[*a]) % sum;
        let rb = (total * counts[*b]) % sum;
        rb.cmp(&ra).then(a.cmp(b))
    });
    for i in order {
        if left == 0 {
            break;
        }
        if parts[i] < counts[i] {
            parts[i] += 1;
            left -= 1;
        }
    }
    parts
}

/// The first prompt of a Claude Code session, if the transcript is still on
/// this machine.
fn claude_prompt(workdir: &Path, session_id: &str, filter: &ExcerptFilter) -> Option<String> {
    prompt::lookup(&prompt::projects_dir()?, workdir, session_id, filter)
}

/// `privacy.redact` from the repository's own config. A config we cannot read
/// redacts nothing, which matches how the rest of madelog treats a missing file.
fn redaction_filter(workdir: Option<&Path>) -> ExcerptFilter {
    let Some(workdir) = workdir else {
        return ExcerptFilter::default();
    };
    Config::load(&workdir.join(".madelog").join("config.toml"))
        .ok()
        .and_then(|config| ExcerptFilter::new(&config.privacy.redact).ok())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// warnings
// ---------------------------------------------------------------------------

/// Say once that Git AI is missing — and only when it really is. A commit
/// with no AI in it has no note either, and that is not worth a word.
fn warn_git_ai_missing() {
    static ONCE: Once = Once::new();
    if git_ai_installed() {
        return;
    }
    ONCE.call_once(|| {
        eprintln!("madelog: Git AI is not installed, so no AI sessions are recorded.");
        eprintln!("madelog: install it with: {GIT_AI_INSTALL}");
    });
}

fn warn_unreadable(reason: &str) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        eprintln!(
            "madelog: a Git AI note could not be read, so its AI sessions are skipped ({reason})"
        );
    });
}

/// How to install Git AI. It ships an installer script rather than a package —
/// `cargo install git-ai` is an unrelated crate — so every hint we print is the
/// command from usegitai.com, not a download page.
#[cfg(not(windows))]
pub const GIT_AI_INSTALL: &str = "curl -sSL https://usegitai.com/install.sh | bash";
/// See [`GIT_AI_INSTALL`].
#[cfg(windows)]
pub const GIT_AI_INSTALL: &str = "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://usegitai.com/install.ps1 | iex\"";

/// Is `git-ai` on PATH, or where its installer puts it?
///
/// Cached: the hook asks once per commit, and walking PATH is not free.
pub fn git_ai_installed() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| git_ai_path().is_some())
}

/// Where `git-ai` actually is, so `madelog doctor` can name the file rather
/// than assert that one exists. Not cached: doctor asks once, and asks after
/// the person has just installed it.
pub fn git_ai_path() -> Option<std::path::PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["git-ai.exe", "git-ai.cmd"]
    } else {
        &["git-ai"]
    };

    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in names {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    // The installer drops the binary here and symlinks it onto PATH; a git
    // hook does not always run with that PATH.
    let bin = dirs::home_dir()?.join(".git-ai").join("bin");
    names
        .iter()
        .map(|n| bin.join(n))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_map_to_ours() {
        assert_eq!(map_tool("claude"), "claude-code");
        assert_eq!(map_tool("github-copilot"), "copilot");
        assert_eq!(map_tool("github-copilot-cli"), "copilot");
        assert_eq!(map_tool("cursor"), "cursor");
        assert_eq!(map_tool("droid"), "droid", "unknown tools keep their name");
        assert_eq!(map_tool("other"), "other");
    }

    #[test]
    fn nothing_to_spread_spreads_nothing() {
        assert_eq!(spread_proportionally(0, &[3, 7]), [0, 0]);
        assert_eq!(spread_proportionally(5, &[]), Vec::<u64>::new());
        assert_eq!(spread_proportionally(5, &[0, 0]), [0, 0]);
    }

    #[test]
    fn a_spread_adds_up_and_never_exceeds_a_file() {
        assert_eq!(spread_proportionally(5, &[5, 10]), [2, 3]);
        assert_eq!(spread_proportionally(3, &[1, 1, 1]), [1, 1, 1]);
        assert_eq!(spread_proportionally(99, &[2, 3]), [2, 3], "capped");
        let parts = spread_proportionally(7, &[10, 1, 4]);
        assert_eq!(parts.iter().sum::<u64>(), 7);
        assert!(parts.iter().zip([10, 1, 4]).all(|(p, n)| *p <= n));
    }

    #[test]
    fn the_reader_reports_no_sessions_without_a_notes_ref() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        assert!(sessions_for_commit(&repo, &"a".repeat(40))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_sha_that_is_not_a_sha_is_an_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        assert!(sessions_for_commit(&repo, "not-a-sha").is_err());
    }

    #[test]
    fn a_note_with_only_human_lines_produces_no_sessions() {
        let parsed = note::parse(
            r#"a.rs
  h_abc 1-4
---
{"schema_version":"authorship/3.0.0","humans":{"h_abc":{"author":"A <a@b.c>"}}}"#,
        )
        .unwrap();
        assert!(to_sessions(&parsed, None).is_empty());
    }

    #[test]
    fn a_session_without_an_id_falls_back_to_its_key() {
        let parsed = note::parse(
            r#"a.rs
  s_ff00::t_11 1-3
---
{"schema_version":"authorship/3.0.0","sessions":{"s_ff00":{"agent_id":{"tool":"amp"}}}}"#,
        )
        .unwrap();
        let sessions = to_sessions(&parsed, None);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "s_ff00");
        assert_eq!(sessions[0].tool, "amp");
        assert_eq!(sessions[0].model, None);
        assert!(sessions[0].accepted);
    }
}
