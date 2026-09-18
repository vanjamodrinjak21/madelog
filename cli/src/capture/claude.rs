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
//! only ever used as a prefilter: `history.jsonl` carries the real path, and
//! every transcript record repeats it in `cwd`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{
    for_each_line, from_epoch_millis, home_override, is_meaningful, is_within, jsonl_files,
    mode_of, overlaps, parse_ts, shorten, text_lines, Adapter, CapturedSession, Detection, Write,
    WriteKind,
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
        let ids = enumerate(repo_root, since, until);
        let mut out = Vec::new();
        for id in ids {
            if let Some(session) = read_session(&projects, repo_root, &id) {
                if overlaps(session.started, session.ended, since, until) {
                    out.push(session);
                }
            }
        }
        out
    }

    fn touching(&self, repo_root: &Path, since: DateTime<Utc>, until: DateTime<Utc>) -> usize {
        // `history.jsonl` alone answers this, and it is one small file.
        direct_session_ids(repo_root, since, until).len()
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

/// Every session id worth opening for this repository and window.
///
/// Sessions whose `project` is the repository come straight out of
/// `history.jsonl`. Sessions run in a git worktree name the worktree instead,
/// so every other session in the window is peeked at for a `worktree-state`
/// record pointing back here.
fn enumerate(repo_root: &Path, since: DateTime<Utc>, until: DateTime<Utc>) -> Vec<String> {
    let mut ids = direct_session_ids(repo_root, since, until);
    for id in worktree_session_ids(repo_root, since, until) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// Session ids whose `history.jsonl` entries name this repository.
fn direct_session_ids(repo_root: &Path, since: DateTime<Utc>, until: DateTime<Utc>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in history_entries(since, until) {
        if is_within(repo_root, Path::new(&entry.project)) && !out.contains(&entry.session_id) {
            out.push(entry.session_id);
        }
    }
    out
}

/// Session ids from somewhere else in the window that fold back to this
/// repository through a `worktree-state` record.
fn worktree_session_ids(
    repo_root: &Path,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Vec<String> {
    let Some(projects) = projects_dir() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for entry in history_entries(since, until) {
        if is_within(repo_root, Path::new(&entry.project)) || seen.contains(&entry.session_id) {
            continue;
        }
        seen.push(entry.session_id.clone());
        if transcripts_for(&projects, &entry.session_id)
            .iter()
            .any(|path| folds_into(path, repo_root))
        {
            out.push(entry.session_id);
        }
    }
    out
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

#[derive(Debug, Clone)]
struct HistoryEntry {
    session_id: String,
    project: String,
}

/// Every `history.jsonl` line whose timestamp falls in the window.
fn history_entries(since: DateTime<Utc>, until: DateTime<Utc>) -> Vec<HistoryEntry> {
    let Some(path) = history_path() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for_each_line(&path, |line| {
        if let Some(value) = json(line) {
            let at = value
                .get("timestamp")
                .and_then(Value::as_i64)
                .and_then(from_epoch_millis);
            let project = value.get("project").and_then(Value::as_str);
            let session_id = value.get("sessionId").and_then(Value::as_str);
            if let (Some(at), Some(project), Some(session_id)) = (at, project, session_id) {
                if at >= since && at <= until {
                    out.push(HistoryEntry {
                        session_id: session_id.to_string(),
                        project: project.to_string(),
                    });
                }
            }
        }
        true
    });
    out
}

/// Every file that belongs to one session: the transcript itself and every
/// subagent transcript under it.
fn transcripts_for(projects: &Path, session_id: &str) -> Vec<PathBuf> {
    if !is_safe_session_id(session_id) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(projects) else {
        return out;
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let main = dir.join(format!("{session_id}.jsonl"));
        if main.is_file() {
            out.push(main);
        }
        let subagents = dir.join(session_id).join("subagents");
        if subagents.is_dir() {
            out.extend(jsonl_files(&subagents, SUBAGENT_DEPTH));
        }
    }
    out
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
    first_prompt: Option<String>,
    prompt_count: u64,
    models: Vec<String>,
    title: Option<String>,
    ai_title: Option<String>,
    lines_added: Option<u64>,
    first_ts: Option<DateTime<Utc>>,
    last_ts: Option<DateTime<Utc>>,
    /// Keyed by absolute path, so several edits of one file are one write.
    writes: BTreeMap<PathBuf, Write>,
    confirmed_cwd: bool,
}

/// Read every file of one session and fold it into a [`CapturedSession`].
///
/// Returns `None` when there is no transcript, or when nothing in it confirms
/// the session really ran in this repository.
fn read_session(projects: &Path, repo_root: &Path, session_id: &str) -> Option<CapturedSession> {
    let files = transcripts_for(projects, session_id);
    if files.is_empty() {
        return None;
    }
    let mut builder = Builder::default();
    for file in &files {
        read_transcript(file, repo_root, &mut builder);
    }
    if !builder.confirmed_cwd {
        return None;
    }
    let started = builder.first_ts?;
    let ended = builder.last_ts.unwrap_or(started);

    let mut writes: Vec<Write> = builder.writes.into_values().collect();
    writes.sort_by(|a, b| a.path.cmp(&b.path));

    Some(CapturedSession {
        tool: "claude-code".to_string(),
        session_id: session_id.to_string(),
        model: mode_of(&builder.models),
        started,
        ended,
        prompt: builder.first_prompt,
        prompt_count: builder.prompt_count,
        title: builder.title.or(builder.ai_title),
        writes,
        lines_added_hint: builder.lines_added,
    })
}

fn read_transcript(path: &Path, repo_root: &Path, builder: &mut Builder) {
    for_each_line(path, |line| {
        if let Some(value) = json(line) {
            fold_record(&value, repo_root, builder);
        }
        true
    });
}

fn fold_record(value: &Value, repo_root: &Path, builder: &mut Builder) {
    if let Some(at) = value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_ts)
    {
        builder.first_ts = Some(match builder.first_ts {
            Some(t) if t <= at => t,
            _ => at,
        });
        builder.last_ts = Some(match builder.last_ts {
            Some(t) if t >= at => t,
            _ => at,
        });
    }
    if let Some(cwd) = value.get("cwd").and_then(Value::as_str) {
        if is_within(repo_root, Path::new(cwd)) {
            builder.confirmed_cwd = true;
        }
    }

    match value.get("type").and_then(Value::as_str).unwrap_or("") {
        "user" => {
            if let Some(text) = user_prompt(value) {
                builder.prompt_count += 1;
                if builder.first_prompt.is_none() {
                    builder.first_prompt = shorten(&text);
                }
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
        "worktree-state" => {
            if let Some(original) = value.get("originalCwd").and_then(Value::as_str) {
                if is_within(repo_root, Path::new(original)) {
                    builder.confirmed_cwd = true;
                }
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
}
