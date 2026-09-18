//! Reading what the AI tools on this machine wrote down.
//!
//! madelog does not ask another program what the AI did; it reads the tools'
//! own local logs. One [`Adapter`] per tool turns those logs into
//! [`CapturedSession`]s: when a session ran, what was asked, and the exact
//! lines the model wrote to which files. [`attribute`] then matches those
//! lines against a commit, which is the only way a number like "41 of 80
//! lines came from the model" can be honest.
//!
//! The formats are worked out in `docs/superpowers/specs/ai-capture-formats.md`
//! and every adapter is fixture-tested against redacted real shapes.
//!
//! Three rules hold everywhere in here:
//!
//! 1. Nothing is fatal. A missing tool, a renamed directory, a half-written
//!    line: all of them mean "no sessions", never an error. A commit that is
//!    recorded without its sessions beats a commit that is lost.
//! 2. Nothing is loaded whole. Transcripts run to tens of megabytes, so every
//!    file is streamed a line at a time.
//! 3. Nothing leaves the repository. Tool output, original file contents and
//!    files outside the working tree are never kept, and `privacy.redact`
//!    plus `.gitignore` are applied here, at capture time, not later.

pub mod attribute;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::config::Config;
use crate::gitrepo::{ExcerptFilter, Repo};

pub use attribute::{attribute, CommitDiff, CommitFileDiff};

/// The page shows a prompt, not a transcript.
pub const MAX_PROMPT_CHARS: usize = 500;

/// What the Cursor adapter can and cannot do, in one line.
pub const CURSOR_NOTE: &str =
    "Cursor keeps conversation bodies encrypted; madelog records that a session happened";

/// What the Copilot adapter can and cannot do, in one line.
pub const COPILOT_NOTE: &str = "not supported yet";

/// Whether a tool is on this machine, and one line about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub installed: bool,
    /// One sentence for `madelog status`, `doctor` and `onboard`.
    pub note: String,
}

impl Detection {
    pub fn found(note: impl Into<String>) -> Detection {
        Detection {
            installed: true,
            note: note.into(),
        }
    }

    pub fn missing(note: impl Into<String>) -> Detection {
        Detection {
            installed: false,
            note: note.into(),
        }
    }
}

/// Did the write create the file or change one that was already there?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteKind {
    Create,
    Update,
}

impl WriteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            WriteKind::Create => "create",
            WriteKind::Update => "update",
        }
    }
}

/// One file an AI tool wrote, and the lines it put there.
///
/// `lines` are verbatim: exactly the bytes the model produced, with no
/// trimming, so that matching them against a commit's added lines is a fact
/// rather than a guess. They are never the file's previous contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write {
    /// Absolute and canonicalised, so two spellings of the same file are one.
    pub path: PathBuf,
    pub lines: Vec<String>,
    pub kind: WriteKind,
}

/// One session of one AI tool, as its own logs describe it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedSession {
    /// `claude-code | codex | cursor | copilot`
    pub tool: String,
    pub session_id: String,
    pub model: Option<String>,
    pub started: DateTime<Utc>,
    pub ended: DateTime<Utc>,
    /// The first thing the person actually asked, collapsed to one line and
    /// cut to [`MAX_PROMPT_CHARS`]. `None` when redacted or unavailable.
    pub prompt: Option<String>,
    pub prompt_count: u64,
    pub title: Option<String>,
    /// The files this session wrote **inside the window**, and the lines it
    /// put in them.
    pub writes: Vec<Write>,
    /// Whether this tool reports the lines it wrote at all.
    ///
    /// `false` for Cursor and Copilot, where an empty `writes` means the tool
    /// cannot say rather than that nothing was written. The difference is
    /// what decides whether a session with nothing to attribute is still
    /// worth recording against a commit.
    pub writes_recorded: bool,
    /// What the tool itself claims it added, when it keeps a count. Used only
    /// where there is no verbatim text to match — Cursor, today.
    pub lines_added_hint: Option<u64>,
}

/// One tool madelog can read.
///
/// Implementations never fail: `sessions` returns what it could read.
pub trait Adapter {
    fn name(&self) -> &str;
    fn detect(&self) -> Detection;
    fn sessions(
        &self,
        repo_root: &Path,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Vec<CapturedSession>;

    /// How many sessions touch this repository, for the one line `status`,
    /// `doctor` and `onboard` print. Adapters override this with whatever
    /// cheap index they have, because nobody should wait on `status`.
    fn touching(&self, repo_root: &Path, since: DateTime<Utc>, until: DateTime<Utc>) -> usize {
        self.sessions(repo_root, since, until).len()
    }
}

/// How far back the per-repository session count looks. Long enough that a
/// repository worked on last month still reports a number, short enough that
/// reading an index stays instant.
pub const REPORT_LOOKBACK_DAYS: i64 = 90;

/// One tool, as `status`, `doctor` and `onboard` print it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolReport {
    pub name: String,
    pub installed: bool,
    pub note: String,
    /// Sessions touching this repository in the last
    /// [`REPORT_LOOKBACK_DAYS`] days; `None` outside a repository or for a
    /// tool that is not installed.
    pub sessions: Option<usize>,
}

impl ToolReport {
    /// `claude-code: transcripts found in …, 12 sessions touch this repo`
    pub fn line(&self) -> String {
        match self.sessions {
            Some(n) => format!("{}: {}, {}", self.name, self.note, sessions_phrase(n)),
            None => format!("{}: {}", self.name, self.note),
        }
    }
}

/// What every adapter has to say, optionally counted against one repository.
pub fn report(repo_root: Option<&Path>) -> Vec<ToolReport> {
    let until = Utc::now();
    let since = until - chrono::Duration::days(REPORT_LOOKBACK_DAYS);
    adapters()
        .iter()
        .map(|adapter| {
            let detection = adapter.detect();
            let sessions = match (detection.installed, repo_root) {
                (true, Some(root)) => Some(adapter.touching(root, since, until)),
                _ => None,
            };
            ToolReport {
                name: adapter.name().to_string(),
                installed: detection.installed,
                note: detection.note,
                sessions,
            }
        })
        .collect()
}

/// Every adapter madelog ships, in the order they are printed.
pub fn adapters() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(claude::ClaudeCode),
        Box::new(codex::Codex),
        Box::new(cursor::Cursor),
        Box::new(copilot::Copilot),
    ]
}

/// What every adapter says about this machine, for `status`, `doctor` and
/// `onboard`.
pub fn detect_all() -> Vec<(String, Detection)> {
    adapters()
        .iter()
        .map(|a| (a.name().to_string(), a.detect()))
        .collect()
}

/// Every session every adapter saw in `[since, until]`, already redacted.
///
/// This is the only entry point the rest of madelog uses, so the privacy rules
/// live in exactly one place: a prompt that matches `privacy.redact` is
/// dropped whole, and a write is dropped whole when its path is ignored by
/// git or matches a redact pattern. Nothing is masked — a masked prompt still
/// shows its shape.
pub fn collect(
    repo: &Repo,
    config: &Config,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Vec<CapturedSession> {
    let Ok(root) = repo.workdir() else {
        return Vec::new();
    };
    let filter = ExcerptFilter::new(&config.privacy.redact).unwrap_or_default();
    let mut out = Vec::new();
    for adapter in adapters() {
        for session in adapter.sessions(&root, since, until) {
            out.push(redact(session, repo, &root, &filter));
        }
    }
    out.sort_by(|a, b| {
        a.started
            .cmp(&b.started)
            .then_with(|| a.tool.cmp(&b.tool))
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    out
}

/// Apply `privacy.redact` and `.gitignore` to one session.
pub fn redact(
    mut session: CapturedSession,
    repo: &Repo,
    root: &Path,
    filter: &ExcerptFilter,
) -> CapturedSession {
    if let Some(prompt) = &session.prompt {
        if !filter.allows(prompt) {
            session.prompt = None;
        }
    }
    session.writes.retain(|write| {
        let Some(rel) = relative(root, &write.path) else {
            // Outside the working tree: not ours to record.
            return false;
        };
        filter.allows(&rel) && !repo.is_ignored(&rel)
    });
    session
}

// ---------------------------------------------------------------------------
// small shared helpers
// ---------------------------------------------------------------------------

/// `path` as a repository-relative, forward-slashed string, or `None` when it
/// is not inside `root`.
pub fn relative(root: &Path, path: &Path) -> Option<String> {
    let root = canonical(root);
    let path = canonical(path);
    let rel = path.strip_prefix(&root).ok()?;
    let text = rel.to_string_lossy().replace('\\', "/");
    (!text.is_empty()).then_some(text)
}

/// A path with every symlink resolved, whether or not the file is still there.
///
/// Plain `canonicalize` fails on a path that does not exist, and a file the AI
/// wrote and the commit then deleted is exactly that. So the deepest ancestor
/// that does exist is canonicalised and the rest of the path put back on.
/// Without this, `/var/folders/...` and `/private/var/folders/...` — the same
/// directory on macOS — never compare equal.
pub fn canonical(path: &Path) -> PathBuf {
    if let Ok(resolved) = path.canonicalize() {
        return resolved;
    }
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path;
    while let Some(parent) = current.parent() {
        match current.file_name() {
            Some(name) => tail.push(name.to_os_string()),
            None => break,
        }
        if let Ok(base) = parent.canonicalize() {
            let mut out = base;
            for part in tail.iter().rev() {
                out.push(part);
            }
            return out;
        }
        current = parent;
    }
    path.to_path_buf()
}

/// Is `path` `root`, or inside it?
pub fn is_within(root: &Path, path: &Path) -> bool {
    let root = canonical(root);
    let path = canonical(path);
    path == root || path.starts_with(&root)
}

/// One line, whitespace collapsed, at most [`MAX_PROMPT_CHARS`] characters.
pub fn shorten(text: &str) -> Option<String> {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.chars().take(MAX_PROMPT_CHARS).collect())
}

/// The lines of a blob of text, without the trailing empty line a final
/// newline produces. Line endings are normalised so a CRLF transcript and a
/// LF working tree still match.
pub fn text_lines(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n")
        .split('\n')
        .map(str::to_string)
        .collect::<Vec<_>>()
        .into_iter()
        .enumerate()
        .filter(|(_, l)| !l.is_empty())
        .map(|(_, l)| l)
        .collect()
}

/// Is this line worth attributing? Blank and whitespace-only lines are the
/// same in every file ever written, so they never count as anyone's work.
pub fn is_meaningful(line: &str) -> bool {
    !line.trim().is_empty()
}

/// The value that appears most often, ties broken by first appearance.
pub fn mode_of(values: &[String]) -> Option<String> {
    let mut counts: Vec<(&String, usize)> = Vec::new();
    for value in values {
        match counts.iter_mut().find(|(v, _)| *v == value) {
            Some((_, n)) => *n += 1,
            None => counts.push((value, 1)),
        }
    }
    // `max_by_key` keeps the last maximum; the first appearance is the one
    // worth naming, so the comparison is strict.
    let mut best: Option<(&String, usize)> = None;
    for (value, n) in counts {
        if best.map_or(true, |(_, top)| n > top) {
            best = Some((value, n));
        }
    }
    best.map(|(v, _)| v.clone())
}

/// Parse a timestamp an AI tool wrote: RFC 3339 with or without a zone.
pub fn parse_ts(raw: &str) -> Option<DateTime<Utc>> {
    if let Ok(t) = DateTime::parse_from_rfc3339(raw) {
        return Some(t.with_timezone(&Utc));
    }
    // Codex writes a local time with an offset; Claude writes Z. Anything
    // else we simply do not know how to read.
    chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()
        .map(|t| t.and_utc())
}

/// Epoch milliseconds, as `~/.claude/history.jsonl` writes them.
pub fn from_epoch_millis(ms: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(ms)
}

/// Do two half-open spans touch?
pub fn overlaps(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> bool {
    end >= since && start <= until
}

/// Read a file line by line, handing each line to `f`, and stop when `f` says
/// so. Never loads the file whole; an unreadable file is simply no lines.
pub fn for_each_line(path: &Path, mut f: impl FnMut(&str) -> bool) {
    use std::io::{BufRead, BufReader};
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let Ok(line) = line else {
            // Not UTF-8: skip the line, keep the file.
            continue;
        };
        if !f(&line) {
            return;
        }
    }
}

/// Every `*.jsonl` under `dir`, recursing at most `depth` levels.
pub fn jsonl_files(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_jsonl(dir, depth, &mut out);
    out.sort();
    out
}

fn collect_jsonl(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth > 0 {
                collect_jsonl(&path, depth - 1, out);
            }
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
}

/// A home directory an environment variable may override, so tests never read
/// a real one.
pub fn home_override(env: &str, fallback: &str) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(env) {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    Some(dirs::home_dir()?.join(fallback))
}

/// "12 sessions touch this repo", or "no sessions touch this repo".
pub fn sessions_phrase(n: usize) -> String {
    match n {
        0 => "no sessions touch this repo".to_string(),
        1 => "1 session touches this repo".to_string(),
        n => format!("{n} sessions touch this repo"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortening_collapses_whitespace_and_cuts_at_five_hundred() {
        assert_eq!(shorten(" a\n  b \t c ").as_deref(), Some("a b c"));
        assert_eq!(shorten("   "), None);
        assert_eq!(shorten(&"x".repeat(900)).unwrap().len(), MAX_PROMPT_CHARS);
    }

    #[test]
    fn text_lines_drop_the_empty_line_a_final_newline_leaves() {
        assert_eq!(text_lines("a\nb\n"), vec!["a", "b"]);
        assert_eq!(text_lines("a\r\nb\r\n"), vec!["a", "b"]);
        assert_eq!(text_lines(""), Vec::<String>::new());
        assert_eq!(text_lines("a\n\nb"), vec!["a", "b"]);
    }

    #[test]
    fn blank_lines_are_nobodys_work() {
        assert!(is_meaningful("fn main() {"));
        assert!(!is_meaningful(""));
        assert!(!is_meaningful("   \t "));
    }

    #[test]
    fn the_mode_is_the_commonest_value() {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(mode_of(&v(&["a", "b", "a"])).as_deref(), Some("a"));
        assert_eq!(mode_of(&v(&["b", "a"])).as_deref(), Some("b"), "first wins");
        assert_eq!(mode_of(&[]), None);
    }

    #[test]
    fn timestamps_are_read_with_or_without_a_zone() {
        assert!(parse_ts("2026-09-18T10:00:00Z").is_some());
        assert!(parse_ts("2026-09-18T12:00:00+02:00").is_some());
        assert!(parse_ts("2026-09-18T10:00:00.123").is_some());
        assert!(parse_ts("yesterday").is_none());
        assert_eq!(
            from_epoch_millis(1_758_103_200_000).unwrap().to_rfc3339(),
            "2025-09-17T10:00:00+00:00"
        );
    }

    #[test]
    fn a_relative_path_is_only_relative_when_it_is_inside() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "").unwrap();

        assert_eq!(
            relative(root, &root.join("src/main.rs")).as_deref(),
            Some("src/main.rs")
        );
        assert_eq!(relative(root, Path::new("/etc/passwd")), None);
        assert_eq!(relative(root, root), None, "the root is not a file in it");
        assert!(is_within(root, &root.join("src")));
        assert!(is_within(root, root));
        assert!(!is_within(root, Path::new("/etc")));
    }

    #[test]
    fn spans_that_do_not_touch_do_not_overlap() {
        let t = |s: &str| parse_ts(s).unwrap();
        let since = t("2026-09-18T10:00:00Z");
        let until = t("2026-09-18T11:00:00Z");
        assert!(overlaps(
            t("2026-09-18T09:00:00Z"),
            t("2026-09-18T10:30:00Z"),
            since,
            until
        ));
        assert!(overlaps(
            t("2026-09-18T10:10:00Z"),
            t("2026-09-18T10:20:00Z"),
            since,
            until
        ));
        assert!(!overlaps(
            t("2026-09-18T08:00:00Z"),
            t("2026-09-18T09:00:00Z"),
            since,
            until
        ));
        assert!(!overlaps(
            t("2026-09-18T12:00:00Z"),
            t("2026-09-18T13:00:00Z"),
            since,
            until
        ));
    }

    #[test]
    fn a_phrase_counts_sessions_in_english() {
        assert_eq!(sessions_phrase(0), "no sessions touch this repo");
        assert_eq!(sessions_phrase(1), "1 session touches this repo");
        assert_eq!(sessions_phrase(12), "12 sessions touch this repo");
    }

    #[test]
    fn every_adapter_has_a_name_and_an_opinion() {
        let names: Vec<String> = adapters().iter().map(|a| a.name().to_string()).collect();
        assert_eq!(names, ["claude-code", "codex", "cursor", "copilot"]);
        for (name, detection) in detect_all() {
            assert!(!detection.note.is_empty(), "{name} said nothing");
        }
    }

    #[test]
    fn jsonl_files_recurse_only_as_deep_as_asked() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
        std::fs::write(dir.path().join("top.jsonl"), "").unwrap();
        std::fs::write(dir.path().join("a/one.jsonl"), "").unwrap();
        std::fs::write(dir.path().join("a/b/c/deep.jsonl"), "").unwrap();
        std::fs::write(dir.path().join("a/not-json.txt"), "").unwrap();

        let shallow = jsonl_files(dir.path(), 1);
        assert_eq!(shallow.len(), 2, "{shallow:?}");
        let deep = jsonl_files(dir.path(), 5);
        assert_eq!(deep.len(), 3, "{deep:?}");
    }
}
