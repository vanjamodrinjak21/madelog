//! Claude Code: the one tool that writes down the exact bytes it produced.
//!
//! Layout on disk (verified 2026-09-18, see the research note):
//!
//! ```text
//! ~/.claude/history.jsonl                      one line per prompt: {display, timestamp, project, sessionId}
//! ~/.claude/projects/<encoded cwd>/<id>.jsonl  the transcript
//! ~/.claude/projects/<encoded cwd>/<id>/subagents/**/*.jsonl
//! ```
//!
//! The directory name is a lossy encoding of the working directory, so it is
//! only ever used as a prefilter: `history.jsonl` carries the real path.
//!
//! Two things about long sessions decide the shape of everything here. A
//! session can keep writing for hours after its last prompt — subagents run
//! on, and the person is not typing while they do — so a session is **never**
//! selected by when it was last spoken to. And one session's files can run to
//! a hundred megabytes, so the files are prefiltered by modification time:
//! a transcript last written before the window began cannot contain a write
//! inside it, and is not opened at all.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{
    for_each_line, from_epoch_millis, home_override, is_meaningful, is_within, jsonl_files,
    mode_of, parse_ts, shorten, text_lines, Adapter, CapturedSession, Detection, Write, WriteKind,
};

/// Overrides `~/.claude`, so tests never read a real transcript.
pub const HOME_ENV: &str = "MADELOG_CLAUDE_HOME";
/// Overrides `~/.claude/projects` alone. Older than [`HOME_ENV`] and still
/// honoured, because the Git AI prompt lookup has used it since day one.
pub const PROJECTS_DIR_ENV: &str = "MADELOG_CLAUDE_PROJECTS_DIR";

/// A worktree session announces itself in its first records. Reading further
/// than this into a transcript that has nothing to do with this repository
/// would cost more than the fold is worth.
const WORKTREE_SCAN_LINES: usize = 500;

/// How deep `subagents/` nests: `subagents/workflows/wf_<id>/agent-<id>.jsonl`.
const SUBAGENT_DEPTH: usize = 4;

/// How much slack the modification-time prefilter allows.
///
/// A file's mtime is when the last line was appended, which is at or after the
/// timestamp inside that line, so the prefilter is already conservative. The
/// margin covers a clock that disagrees with itself by a little.
const MTIME_SLACK_SECONDS: i64 = 120;

pub struct ClaudeCode;

impl Adapter for ClaudeCode {
    fn name(&self) -> &str {
        "claude-code"
    }

    fn detect(&self) -> Detection {
        match (projects_dir(), history_path()) {
            (Some(dir), _) if dir.is_dir() => {
                Detection::found(format!("transcripts found in {}", dir.display()))
            }
            (_, Some(history)) if history.is_file() => {
                Detection::found(format!("history found at {}", history.display()))
            }
            _ => Detection::missing("no transcripts in ~/.claude, so nothing to read"),
        }
    }

    fn sessions(
        &self,
        repo_root: &Path,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Vec<CapturedSession> {
        let Some(projects) = projects_dir() else {
            return Vec::new();
        };
        let dirs = project_dirs(&projects);
        let mut out = Vec::new();
        for found in select(&dirs, repo_root, since, until, Fold::Worktrees) {
            let Some(session) = read_session(&found, repo_root, since, until) else {
                continue;
            };
            // The file prefilter is about what a file *could* hold; this is
            // about what it does. A session that wrote nothing in the window
            // and was not itself running during it did not happen here.
            if !session.writes.is_empty()
                || super::overlaps(session.started, session.ended, since, until)
            {
                out.push(session);
            }
        }
        out
    }

    fn touching(&self, repo_root: &Path, since: DateTime<Utc>, until: DateTime<Utc>) -> usize {
        // `history.jsonl` plus one stat per file. No transcript is opened —
        // which is why this one does not fold worktrees: a count is not worth
        // peeking into every other project's transcripts for.
        let Some(projects) = projects_dir() else {
            return 0;
        };
        select(
            &project_dirs(&projects),
            repo_root,
            since,
            until,
            Fold::None,
        )
        .len()
    }
}

/// `~/.claude`, or whatever [`HOME_ENV`] names.
pub fn claude_home() -> Option<PathBuf> {
    home_override(HOME_ENV, ".claude")
}

/// Where the transcripts are.
pub fn projects_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(PROJECTS_DIR_ENV) {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    Some(claude_home()?.join("projects"))
}

/// Where the one-line-per-prompt index is.
pub fn history_path() -> Option<PathBuf> {
    Some(claude_home()?.join("history.jsonl"))
}

// ---------------------------------------------------------------------------
// (a) enumerate
// ---------------------------------------------------------------------------

/// What `history.jsonl` knows about one session, without opening a transcript.
#[derive(Debug, Clone)]
struct HistorySession {
    id: String,
    project: String,
    /// The first thing the person asked, already shortened.
    first_prompt: Option<String>,
    prompt_count: u64,
    first_at: DateTime<Utc>,
    last_at: DateTime<Utc>,
}

/// Whether a session belonging to another directory is worth peeking into.
///
/// Folding costs a read of every candidate's first records, and over a long
/// window there are hundreds of candidates — every session of every project
/// on the machine. The hook pays it, because its window is a few hours wide
/// and a worktree's work is real work. `madelog status` does not, because it
/// is printing a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fold {
    Worktrees,
    None,
}

/// A session that is worth opening, and the files worth opening for it.
#[derive(Debug, Clone)]
struct Selected {
    session: HistorySession,
    /// The transcripts that could hold a write inside the window: every file
    /// modified at or after the start of it.
    files: Vec<PathBuf>,
}

/// Every session that could have written into this repository in the window.
///
/// Selection is deliberately not about when the person last typed. A session
/// is taken when it had started by the end of the window and at least one of
/// its files was written at or after the start of it — which is exactly the
/// condition for it to be able to carry a write inside the window.
fn select(
    dirs: &[PathBuf],
    repo_root: &Path,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    fold: Fold,
) -> Vec<Selected> {
    let cutoff = since - chrono::Duration::seconds(MTIME_SLACK_SECONDS);
    let mut out = Vec::new();
    for session in history_sessions(until) {
        // The cheap question first: is this session even a candidate? Most
        // of the sessions on a machine belong to other projects, and asking
        // the filesystem about every one of them is what makes this slow.
        let mine = is_within(repo_root, Path::new(&session.project));
        if !mine && fold == Fold::None {
            continue;
        }
        let files = transcripts_for(dirs, &session.id);
        if files.is_empty() {
            continue;
        }
        let recent: Vec<PathBuf> = files
            .iter()
            .filter(|path| modified_at_or_after(path, cutoff))
            .cloned()
            .collect();
        if recent.is_empty() {
            // Nothing in this session has been written since the window
            // opened, so it cannot have contributed to this commit.
            continue;
        }

        if mine {
            out.push(Selected {
                session,
                files: recent,
            });
            continue;
        }
        // Somewhere else — unless it is a worktree of this repository. The
        // `worktree-state` record is near the top of the main transcript, so
        // both it and the recent files are worth a short peek.
        if files
            .iter()
            .take(1)
            .chain(recent.iter())
            .any(|path| folds_into(path, repo_root))
        {
            out.push(Selected {
                session,
                files: recent,
            });
        }
    }
    out
}

/// Every session `history.jsonl` knows about that had started by `until`,
/// with its prompt facts, in the order they first appear.
fn history_sessions(until: DateTime<Utc>) -> Vec<HistorySession> {
    let Some(path) = history_path() else {
        return Vec::new();
    };
    let mut order: Vec<String> = Vec::new();
    let mut by_id: BTreeMap<String, HistorySession> = BTreeMap::new();

    for_each_line(&path, |line| {
        if let Some(value) = json(line) {
            let at = value
                .get("timestamp")
                .and_then(Value::as_i64)
                .and_then(from_epoch_millis);
            let project = value.get("project").and_then(Value::as_str);
            let id = value.get("sessionId").and_then(Value::as_str);
            if let (Some(at), Some(project), Some(id)) = (at, project, id) {
                if at <= until {
                    // A slash command is something you told the tool, not
                    // something you asked it to build. It still marks the
                    // session as active, so it counts for the times.
                    let display = value.get("display").and_then(Value::as_str);
                    let asked = display.filter(|d| !is_slash_command(d));
                    let prompt = asked.and_then(shorten);
                    match by_id.get_mut(id) {
                        Some(found) => {
                            if asked.is_some() {
                                found.prompt_count += 1;
                                if found.first_prompt.is_none() {
                                    found.first_prompt = prompt;
                                }
                            }
                            found.first_at = found.first_at.min(at);
                            found.last_at = found.last_at.max(at);
                        }
                        None => {
                            order.push(id.to_string());
                            by_id.insert(
                                id.to_string(),
                                HistorySession {
                                    id: id.to_string(),
                                    project: project.to_string(),
                                    first_prompt: prompt,
                                    prompt_count: u64::from(asked.is_some()),
                                    first_at: at,
                                    last_at: at,
                                },
                            );
                        }
                    }
                }
            }
        }
        true
    });

    order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect()
}

/// Is this a slash command — `/model fable`, `/done`, `/code-review:pr` —
/// rather than a sentence?
///
/// The first token after the slash is a command name: letters, digits and the
/// punctuation a plugin name uses. A path pasted at the start of a prompt
/// (`/Users/ana/spec.pdf use this`) has another slash in that token, so it is
/// a prompt and is treated as one.
fn is_slash_command(display: &str) -> bool {
    let Some(rest) = display.trim_start().strip_prefix('/') else {
        return false;
    };
    let name = rest.split_whitespace().next().unwrap_or("");
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
}

/// Was this file written at or after `cutoff`? An unreadable mtime counts as
/// yes: better to read a file we did not need than to miss a write.
fn modified_at_or_after(path: &Path, cutoff: DateTime<Utc>) -> bool {
    let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) else {
        return true;
    };
    at_or_after(modified, cutoff)
}

fn at_or_after(modified: SystemTime, cutoff: DateTime<Utc>) -> bool {
    DateTime::<Utc>::from(modified) >= cutoff
}

/// The directories under `~/.claude/projects`, listed once per run.
fn project_dirs(projects: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(projects) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// Does this transcript say it is a worktree of `repo_root`?
fn folds_into(path: &Path, repo_root: &Path) -> bool {
    let mut folds = false;
    let mut read = 0usize;
    for_each_line(path, |line| {
        read += 1;
        if let Some(value) = json(line) {
            if value.get("type").and_then(Value::as_str) == Some("worktree-state") {
                if let Some(original) = value.get("originalCwd").and_then(Value::as_str) {
                    if is_within(repo_root, Path::new(original)) {
                        folds = true;
                        return false;
                    }
                }
            }
        }
        read < WORKTREE_SCAN_LINES
    });
    folds
}

/// Every file that belongs to one session: the transcript itself first, then
/// every subagent transcript under it.
fn transcripts_for(dirs: &[PathBuf], session_id: &str) -> Vec<PathBuf> {
    if !is_safe_session_id(session_id) {
        return Vec::new();
    }
    let mut main = Vec::new();
    let mut subagents = Vec::new();
    for dir in dirs {
        let transcript = dir.join(format!("{session_id}.jsonl"));
        if transcript.is_file() {
            main.push(transcript);
        }
        let nested = dir.join(session_id).join("subagents");
        if nested.is_dir() {
            subagents.extend(jsonl_files(&nested, SUBAGENT_DEPTH));
        }
    }
    main.extend(subagents);
    main
}

/// A session id comes from a file madelog did not write: never let one out of
/// the projects directory.
fn is_safe_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !session_id.starts_with('.')
}

// ---------------------------------------------------------------------------
// (b) + (c) read one session
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Builder {
    /// Only a fallback: the prompt normally comes from `history.jsonl`.
    transcript_prompt: Option<String>,
    models: Vec<String>,
    title: Option<String>,
    ai_title: Option<String>,
    lines_added: Option<u64>,
    first_ts: Option<DateTime<Utc>>,
    last_ts: Option<DateTime<Utc>>,
    /// Keyed by absolute path, so several edits of one file are one write.
    writes: BTreeMap<PathBuf, Write>,
}

/// Read the chosen files of one session and fold them into a
/// [`CapturedSession`].
///
/// The prompt facts come from `history.jsonl`, which already has them, so a
/// thirteen-megabyte transcript is never read for a sentence.
fn read_session(
    found: &Selected,
    repo_root: &Path,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Option<CapturedSession> {
    let mut builder = Builder::default();
    for file in &found.files {
        read_transcript(file, repo_root, since, until, &mut builder);
    }

    let history = &found.session;
    let started = match builder.first_ts {
        Some(at) => at.min(history.first_at),
        None => history.first_at,
    };
    let ended = match builder.last_ts {
        Some(at) => at.max(history.last_at),
        None => history.last_at,
    };

    let mut writes: Vec<Write> = builder.writes.into_values().collect();
    writes.sort_by(|a, b| a.path.cmp(&b.path));

    Some(CapturedSession {
        tool: "claude-code".to_string(),
        session_id: history.id.clone(),
        model: mode_of(&builder.models),
        started,
        ended,
        prompt: history.first_prompt.clone().or(builder.transcript_prompt),
        prompt_count: history.prompt_count,
        title: builder.title.or(builder.ai_title),
        writes,
        writes_recorded: true,
        lines_added_hint: builder.lines_added,
    })
}

fn read_transcript(
    path: &Path,
    repo_root: &Path,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    builder: &mut Builder,
) {
    for_each_line(path, |line| {
        if let Some(value) = json(line) {
            fold_record(&value, repo_root, since, until, builder);
        }
        true
    });
}

fn fold_record(
    value: &Value,
    repo_root: &Path,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    builder: &mut Builder,
) {
    let at = value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_ts);
    if let Some(at) = at {
        builder.first_ts = Some(match builder.first_ts {
            Some(t) if t <= at => t,
            _ => at,
        });
        builder.last_ts = Some(match builder.last_ts {
            Some(t) if t >= at => t,
            _ => at,
        });
    }

    match value.get("type").and_then(Value::as_str).unwrap_or("") {
        "user" => {
            // `history.jsonl` has the prompts, so this is only for a session
            // it does not know about — an older Claude, or a history file
            // that has been rotated.
            if builder.transcript_prompt.is_none() {
                if let Some(text) = user_prompt(value) {
                    builder.transcript_prompt = shorten(&text);
                }
            }
            // A write belongs to the commit that was made after it, so a
            // record from outside the window is another commit's business.
            if !in_window(at, since, until) {
                return;
            }
            if let Some((path, write)) = tool_write(value, repo_root) {
                merge_write(&mut builder.writes, path, write);
            }
        }
        "assistant" => {
            if let Some(model) = value
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
                .filter(|m| !m.trim().is_empty())
            {
                builder.models.push(model.to_string());
            }
        }
        "custom-title" => {
            builder.title = title_of(value).or(builder.title.take());
        }
        "ai-title" => {
            if builder.ai_title.is_none() {
                builder.ai_title = title_of(value);
            }
        }
        "cost-state" => {
            if let Some(n) = value.get("totalLinesAdded").and_then(Value::as_u64) {
                builder.lines_added = Some(n);
            }
        }
        _ => {}
    }
}

/// A record with no timestamp at all is kept: the alternative is losing a
/// write because one field was missing.
fn in_window(at: Option<DateTime<Utc>>, since: DateTime<Utc>, until: DateTime<Utc>) -> bool {
    match at {
        Some(at) => at >= since && at <= until,
        None => true,
    }
}

fn title_of(value: &Value) -> Option<String> {
    value
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// The person's own words, if this record is a prompt at all.
///
/// Sidechain records belong to a subagent, `isMeta` records are injected
/// system reminders, a compact summary is the model's own recap, and a
/// `message.content` array is a tool result. None of them is a prompt.
///
/// A fallback only: the prompt madelog shows normally comes from
/// `history.jsonl`, which carries the same text without opening a transcript.
fn user_prompt(value: &Value) -> Option<String> {
    if value.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    if value.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    if value.get("isCompactSummary").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let text = value.get("message")?.get("content")?.as_str()?;
    (!text.trim().is_empty()).then(|| text.to_string())
}

/// The file this record says landed, and the lines that landed in it.
///
/// The applied outcome lives in the top-level `toolUseResult`: a dict with a
/// `filePath` means the write happened, a string starting `Error:` means it
/// did not. Tool stdout and the file's previous contents are in there too,
/// and neither is ever read.
fn tool_write(value: &Value, repo_root: &Path) -> Option<(PathBuf, Write)> {
    if tool_result_failed(value) {
        return None;
    }
    let result = value.get("toolUseResult")?.as_object()?;
    let file_path = result.get("filePath")?.as_str()?;
    let path = super::canonical(Path::new(file_path));
    if !is_within(repo_root, &path) {
        return None;
    }

    let mut lines: Vec<String> = Vec::new();
    // Write: the whole file as the model wrote it.
    if let Some(content) = result.get("content").and_then(Value::as_str) {
        lines = text_lines(content);
    }
    // Edit: the replacement text.
    if let Some(new_string) = result.get("newString").and_then(Value::as_str) {
        merge_max(&mut lines, text_lines(new_string));
    }
    // Both carry the hunks that were applied; the `+` lines are the truth
    // about what the file gained.
    merge_max(&mut lines, patch_added_lines(result.get("structuredPatch")));

    lines.retain(|l| is_meaningful(l));
    if lines.is_empty() {
        return None;
    }

    let kind = match result.get("type").and_then(Value::as_str) {
        Some("create") => WriteKind::Create,
        _ => WriteKind::Update,
    };
    Some((path.clone(), Write { path, lines, kind }))
}

/// Did the tool report an error? Then nothing was written.
fn tool_result_failed(value: &Value) -> bool {
    if value
        .get("toolUseResult")
        .is_some_and(|r| r.as_str().is_some())
    {
        // A string `toolUseResult` is Claude Code's failure shape.
        return true;
    }
    let Some(blocks) = value
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    blocks
        .iter()
        .any(|b| b.get("is_error").and_then(Value::as_bool) == Some(true))
}

/// The `+` lines of a `structuredPatch`, without the marker.
fn patch_added_lines(patch: Option<&Value>) -> Vec<String> {
    let Some(hunks) = patch.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for hunk in hunks {
        let Some(lines) = hunk.get("lines").and_then(Value::as_array) else {
            continue;
        };
        for line in lines.iter().filter_map(Value::as_str) {
            if let Some(added) = line.strip_prefix('+') {
                out.push(added.to_string());
            }
        }
    }
    out
}

/// Fold `more` into `lines` so that each distinct line appears as often as the
/// fuller of the two sources says, never as often as both together.
///
/// An `Edit` reports its replacement twice — once as `newString`, once as the
/// `+` lines of the patch — and counting a line twice for that would inflate
/// every attribution built on it.
fn merge_max(lines: &mut Vec<String>, more: Vec<String>) {
    let mut wanted: BTreeMap<&str, usize> = BTreeMap::new();
    for line in &more {
        *wanted.entry(line.as_str()).or_default() += 1;
    }
    let mut extra: Vec<String> = Vec::new();
    for (line, want) in wanted {
        let have = lines.iter().filter(|l| l.as_str() == line).count();
        for _ in have..want {
            extra.push(line.to_string());
        }
    }
    lines.extend(extra);
}

/// Several edits of one file in one session are one write: the file only ever
/// held one copy of a line at a time, however many times it was written.
fn merge_write(writes: &mut BTreeMap<PathBuf, Write>, path: PathBuf, write: Write) {
    match writes.get_mut(&path) {
        Some(existing) => {
            merge_max(&mut existing.lines, write.lines);
            // Creating and then updating is still a creation.
            if write.kind == WriteKind::Create {
                existing.kind = WriteKind::Create;
            }
        }
        None => {
            writes.insert(path, write);
        }
    }
}

fn json(line: &str) -> Option<Value> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    serde_json::from_str(line).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(raw: &str) -> Value {
        serde_json::from_str(raw).unwrap()
    }

    #[test]
    fn only_real_user_prompts_count() {
        assert_eq!(
            user_prompt(&v(r#"{"message":{"content":"build the thing"}}"#)).as_deref(),
            Some("build the thing")
        );
        assert_eq!(
            user_prompt(&v(r#"{"isSidechain":true,"message":{"content":"x"}}"#)),
            None
        );
        assert_eq!(
            user_prompt(&v(r#"{"isMeta":true,"message":{"content":"x"}}"#)),
            None
        );
        assert_eq!(
            user_prompt(&v(r#"{"isCompactSummary":true,"message":{"content":"x"}}"#)),
            None
        );
        assert_eq!(
            user_prompt(&v(
                r#"{"message":{"content":[{"type":"tool_result","content":"ok"}]}}"#
            )),
            None,
            "an array is a tool result, not a prompt"
        );
        assert_eq!(user_prompt(&v(r#"{"message":{"content":"  "}}"#)), None);
    }

    #[test]
    fn a_write_result_gives_its_content_verbatim() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("a.rs");
        std::fs::write(&file, "x").unwrap();
        let record = serde_json::json!({
            "type": "user",
            "toolUseResult": {
                "type": "create",
                "filePath": file.to_string_lossy(),
                "content": "fn main() {\n\n    println!(\"hi\");\n}\n",
            }
        });
        let (path, write) = tool_write(&record, dir.path()).unwrap();
        assert_eq!(path, super::super::canonical(&file));
        assert_eq!(write.kind, WriteKind::Create);
        assert_eq!(write.lines, ["fn main() {", "    println!(\"hi\");", "}"]);
    }

    #[test]
    fn an_edit_result_gives_new_string_and_plus_lines_once_each() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("a.rs");
        std::fs::write(&file, "x").unwrap();
        let record = serde_json::json!({
            "type": "user",
            "toolUseResult": {
                "filePath": file.to_string_lossy(),
                "oldString": "let a = 1;",
                "newString": "let a = 2;\nlet b = 3;",
                "structuredPatch": [{
                    "lines": [" ctx", "-let a = 1;", "+let a = 2;", "+let b = 3;", "+let c = 4;"]
                }],
            }
        });
        let (_, write) = tool_write(&record, dir.path()).unwrap();
        assert_eq!(write.kind, WriteKind::Update);
        assert_eq!(write.lines, ["let a = 2;", "let b = 3;", "let c = 4;"]);
    }

    #[test]
    fn a_failed_tool_result_writes_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("a.rs");
        let as_error = serde_json::json!({
            "type": "user",
            "message": {"content": [{"type": "tool_result", "is_error": true}]},
            "toolUseResult": {"filePath": file.to_string_lossy(), "content": "x\n"},
        });
        assert!(tool_write(&as_error, dir.path()).is_none());

        let as_string = serde_json::json!({
            "type": "user",
            "toolUseResult": "Error: file not found",
        });
        assert!(tool_write(&as_string, dir.path()).is_none());
    }

    #[test]
    fn a_write_outside_the_repository_is_not_ours() {
        let dir = tempfile::TempDir::new().unwrap();
        let record = serde_json::json!({
            "type": "user",
            "toolUseResult": {"filePath": "/etc/hosts", "content": "127.0.0.1 x\n"},
        });
        assert!(tool_write(&record, dir.path()).is_none());
    }

    #[test]
    fn blank_lines_never_become_a_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let record = serde_json::json!({
            "type": "user",
            "toolUseResult": {"filePath": dir.path().join("a.rs").to_string_lossy(),
                              "content": "\n   \n\t\n"},
        });
        assert!(tool_write(&record, dir.path()).is_none());
    }

    #[test]
    fn session_ids_that_could_escape_the_directory_are_refused() {
        assert!(is_safe_session_id("aa2b7669-72a3-4ce8-8fbf-62fd9caee4f5"));
        assert!(!is_safe_session_id("../../etc/passwd"));
        assert!(!is_safe_session_id("a/b"));
        assert!(!is_safe_session_id(".hidden"));
        assert!(!is_safe_session_id(""));
    }

    #[test]
    fn plus_lines_lose_their_marker_and_context_lines_stay_out() {
        let patch = v(r#"[{"lines":[" keep","+added","-gone","+more"]}]"#);
        assert_eq!(patch_added_lines(Some(&patch)), ["added", "more"]);
        assert!(patch_added_lines(None).is_empty());
    }

    #[test]
    fn merging_never_counts_the_same_line_twice() {
        let mut lines = vec!["a".to_string(), "b".to_string()];
        merge_max(&mut lines, vec!["b".to_string(), "c".to_string()]);
        assert_eq!(lines, ["a", "b", "c"]);
    }

    #[test]
    fn a_slash_command_is_not_a_prompt() {
        assert!(is_slash_command("/model fable"));
        assert!(is_slash_command("/done"));
        assert!(is_slash_command("/code-review:code-review"));
        assert!(is_slash_command("  /clear"));
        assert!(!is_slash_command("build the parser"));
        assert!(
            !is_slash_command("'/Users/ana/spec.pdf' use this and write the plan"),
            "a quoted path is not a command"
        );
        assert!(
            !is_slash_command("/Users/ana/spec.pdf use this"),
            "a bare path has another slash in its first token"
        );
        assert!(!is_slash_command("/"));
        assert!(!is_slash_command(""));
    }

    #[test]
    fn a_record_is_in_the_window_by_its_own_timestamp() {
        let t = |s: &str| parse_ts(s).unwrap();
        let since = t("2026-09-18T11:00:00Z");
        let until = t("2026-09-18T13:00:00Z");
        assert!(in_window(Some(t("2026-09-18T12:00:00Z")), since, until));
        assert!(in_window(Some(since), since, until), "inclusive");
        assert!(in_window(Some(until), since, until), "inclusive");
        assert!(!in_window(Some(t("2026-09-18T10:59:59Z")), since, until));
        assert!(!in_window(Some(t("2026-09-18T13:00:01Z")), since, until));
        assert!(
            in_window(None, since, until),
            "a record with no timestamp is kept rather than lost"
        );
    }

    #[test]
    fn the_mtime_prefilter_allows_a_little_slack() {
        let cutoff = parse_ts("2026-09-18T12:00:00Z").unwrap();
        let at = |s: &str| SystemTime::from(parse_ts(s).unwrap());
        assert!(at_or_after(at("2026-09-18T12:00:00Z"), cutoff));
        assert!(at_or_after(at("2026-09-18T12:00:01Z"), cutoff));
        assert!(!at_or_after(at("2026-09-18T11:59:59Z"), cutoff));
    }

    #[test]
    fn a_file_whose_mtime_cannot_be_read_is_read_anyway() {
        let dir = tempfile::TempDir::new().unwrap();
        let gone = dir.path().join("not-there.jsonl");
        assert!(
            modified_at_or_after(&gone, Utc::now()),
            "missing rather than old: never drop a write over a stat"
        );
    }
}
