//! Codex CLI: date-partitioned rollout files, one JSON object per line.
//!
//! ```text
//! ~/.codex/sessions/YYYY/MM/DD/rollout-<ISO>-<uuid>.jsonl
//! ```
//!
//! There is no path encoding to reverse: line 0 of every rollout is a
//! `session_meta` record carrying the real `cwd`, so enumeration is one short
//! read per file. Every line is `{timestamp, ordinal, type, payload}`.
//!
//! The gap to know about: a write done through a shell heredoc shows up as a
//! `CommandExecution` and never as a `FileChange`, so madelog does not see it.
//! Nothing is invented to cover that; the lines simply count as human.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{
    for_each_line, home_override, is_meaningful, is_within, jsonl_files, overlaps, parse_ts,
    shorten, text_lines, Adapter, CapturedSession, Detection, Write, WriteKind,
};

/// Overrides `~/.codex`, so tests never read a real rollout.
pub const HOME_ENV: &str = "MADELOG_CODEX_HOME";

/// `sessions/YYYY/MM/DD/*.jsonl`.
const SESSIONS_DEPTH: usize = 4;

pub struct Codex;

impl Adapter for Codex {
    fn name(&self) -> &str {
        "codex"
    }

    fn detect(&self) -> Detection {
        match sessions_dir() {
            Some(dir) if dir.is_dir() => {
                Detection::found(format!("rollouts found in {}", dir.display()))
            }
            _ => Detection::missing("no rollouts in ~/.codex, so nothing to read"),
        }
    }

    fn sessions(
        &self,
        repo_root: &Path,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Vec<CapturedSession> {
        let mut out = Vec::new();
        for path in rollouts_for(repo_root) {
            if let Some(session) = read_rollout(&path, repo_root) {
                if overlaps(session.started, session.ended, since, until) {
                    out.push(session);
                }
            }
        }
        out
    }

    fn touching(&self, repo_root: &Path, _since: DateTime<Utc>, _until: DateTime<Utc>) -> usize {
        // One line per file answers "is this repo's", which is the question.
        rollouts_for(repo_root).len()
    }
}

/// `~/.codex`, or whatever [`HOME_ENV`] names.
pub fn codex_home() -> Option<PathBuf> {
    home_override(HOME_ENV, ".codex")
}

pub fn sessions_dir() -> Option<PathBuf> {
    Some(codex_home()?.join("sessions"))
}

/// Every rollout whose `session_meta` names this repository.
fn rollouts_for(repo_root: &Path) -> Vec<PathBuf> {
    let Some(dir) = sessions_dir() else {
        return Vec::new();
    };
    jsonl_files(&dir, SESSIONS_DEPTH)
        .into_iter()
        .filter(|path| meta_cwd(path).is_some_and(|cwd| is_within(repo_root, Path::new(&cwd))))
        .collect()
}

/// The `cwd` of line 0, without reading the rest of the file.
fn meta_cwd(path: &Path) -> Option<String> {
    let mut cwd = None;
    for_each_line(path, |line| {
        if let Some(value) = json(line) {
            if value.get("type").and_then(Value::as_str) == Some("session_meta") {
                cwd = payload(&value)
                    .and_then(|p| p.get("cwd"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
        }
        // Only the first line can be the meta record.
        false
    });
    cwd
}

fn payload(value: &Value) -> Option<&Value> {
    value.get("payload")
}

#[derive(Default)]
struct Builder {
    session_id: Option<String>,
    first_prompt: Option<String>,
    prompt_count: u64,
    model: Option<String>,
    first_ts: Option<DateTime<Utc>>,
    last_ts: Option<DateTime<Utc>>,
    writes: BTreeMap<PathBuf, Write>,
}

fn read_rollout(path: &Path, repo_root: &Path) -> Option<CapturedSession> {
    let mut builder = Builder::default();
    for_each_line(path, |line| {
        if let Some(value) = json(line) {
            fold_record(&value, repo_root, &mut builder);
        }
        true
    });

    let session_id = builder.session_id.or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
    })?;
    let started = builder.first_ts?;
    let ended = builder.last_ts.unwrap_or(started);
    let mut writes: Vec<Write> = builder.writes.into_values().collect();
    writes.sort_by(|a, b| a.path.cmp(&b.path));

    Some(CapturedSession {
        tool: "codex".to_string(),
        session_id,
        model: builder.model,
        started,
        ended,
        prompt: builder.first_prompt,
        prompt_count: builder.prompt_count,
        title: None,
        writes,
        lines_added_hint: None,
    })
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

    match value.get("type").and_then(Value::as_str).unwrap_or("") {
        "session_meta" => {
            if let Some(p) = payload(value) {
                builder.session_id = p
                    .get("session_id")
                    .or_else(|| p.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
        }
        "turn_context" => {
            if let Some(model) = payload(value)
                .and_then(|p| p.get("model"))
                .and_then(Value::as_str)
                .filter(|m| !m.trim().is_empty())
            {
                builder.model = Some(model.to_string());
            }
        }
        "event_msg" => {
            let Some(p) = payload(value) else { return };
            if p.get("type").and_then(Value::as_str) != Some("item_completed") {
                return;
            }
            let Some(item) = p.get("item") else { return };
            match item_type(item) {
                Some("UserMessage") => {
                    if let Some(text) = user_message(item) {
                        builder.prompt_count += 1;
                        if builder.first_prompt.is_none() {
                            builder.first_prompt = shorten(&text);
                        }
                    }
                }
                Some("FileChange") => {
                    for (path, write) in file_changes(item, repo_root) {
                        merge_write(&mut builder.writes, path, write);
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Codex tags an item either with a `type` field or with a single-key
/// wrapper; both shapes appear in the wild.
fn item_type(item: &Value) -> Option<&str> {
    if let Some(t) = item.get("type").and_then(Value::as_str) {
        return Some(t);
    }
    item.as_object()?.keys().next().map(String::as_str)
}

fn item_body(item: &Value) -> &Value {
    if item.get("type").is_some() {
        return item;
    }
    match item.as_object().and_then(|o| o.values().next()) {
        Some(inner) => inner,
        None => item,
    }
}

fn user_message(item: &Value) -> Option<String> {
    let body = item_body(item);
    if let Some(text) = body.get("text").and_then(Value::as_str) {
        return (!text.trim().is_empty()).then(|| text.to_string());
    }
    let blocks = body.get("content")?.as_array()?;
    let joined: Vec<&str> = blocks
        .iter()
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect();
    let text = joined.join(" ");
    (!text.trim().is_empty()).then_some(text)
}

/// `changes: {<abs path>: {type: "add", content} | {type: "update", unified_diff}}`.
fn file_changes(item: &Value, repo_root: &Path) -> Vec<(PathBuf, Write)> {
    let body = item_body(item);
    let Some(changes) = body.get("changes").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (raw_path, change) in changes {
        let path = super::canonical(Path::new(raw_path));
        if !is_within(repo_root, &path) {
            continue;
        }
        let (kind, mut lines) = match change.get("type").and_then(Value::as_str) {
            Some("add") => (
                WriteKind::Create,
                text_lines(change.get("content").and_then(Value::as_str).unwrap_or("")),
            ),
            _ => (
                WriteKind::Update,
                diff_added_lines(
                    change
                        .get("unified_diff")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                ),
            ),
        };
        lines.retain(|l| is_meaningful(l));
        if lines.is_empty() {
            continue;
        }
        out.push((path.clone(), Write { path, lines, kind }));
    }
    out
}

/// The `+` lines of a unified diff, without the marker and without the `+++`
/// header.
pub fn diff_added_lines(diff: &str) -> Vec<String> {
    diff.replace("\r\n", "\n")
        .lines()
        .filter(|l| !l.starts_with("+++"))
        .filter_map(|l| l.strip_prefix('+'))
        .map(str::to_string)
        .collect()
}

fn merge_write(writes: &mut BTreeMap<PathBuf, Write>, path: PathBuf, write: Write) {
    match writes.get_mut(&path) {
        Some(existing) => {
            for line in write.lines {
                if !existing.lines.contains(&line) {
                    existing.lines.push(line);
                }
            }
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

    #[test]
    fn a_unified_diff_gives_up_its_added_lines() {
        let diff = "--- a/x.rs\n+++ b/x.rs\n@@ -1,2 +1,3 @@\n ctx\n-gone\n+kept\n+also\n";
        assert_eq!(diff_added_lines(diff), ["kept", "also"]);
        assert!(diff_added_lines("").is_empty());
    }

    #[test]
    fn an_add_change_gives_the_whole_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("new.rs");
        let item = serde_json::json!({
            "type": "FileChange",
            "changes": {
                file.to_string_lossy(): {"type": "add", "content": "fn a() {}\n\nfn b() {}\n"}
            }
        });
        let writes = file_changes(&item, dir.path());
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].1.kind, WriteKind::Create);
        assert_eq!(writes[0].1.lines, ["fn a() {}", "fn b() {}"]);
    }

    #[test]
    fn an_update_change_gives_the_plus_lines() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("x.rs");
        let item = serde_json::json!({
            "type": "FileChange",
            "changes": {
                file.to_string_lossy(): {
                    "type": "update",
                    "unified_diff": "@@ -1 +1,2 @@\n ctx\n+let a = 1;\n"
                }
            }
        });
        let writes = file_changes(&item, dir.path());
        assert_eq!(writes[0].1.kind, WriteKind::Update);
        assert_eq!(writes[0].1.lines, ["let a = 1;"]);
    }

    #[test]
    fn a_change_outside_the_repository_is_dropped() {
        let dir = tempfile::TempDir::new().unwrap();
        let item = serde_json::json!({
            "type": "FileChange",
            "changes": {"/etc/hosts": {"type": "add", "content": "x\n"}}
        });
        assert!(file_changes(&item, dir.path()).is_empty());
    }

    #[test]
    fn a_user_message_reads_from_text_or_from_content_blocks() {
        let plain = serde_json::json!({"type": "UserMessage", "text": "do the thing"});
        assert_eq!(user_message(&plain).as_deref(), Some("do the thing"));

        let blocks = serde_json::json!({
            "type": "UserMessage",
            "content": [{"text": "do"}, {"text": "it"}]
        });
        assert_eq!(user_message(&blocks).as_deref(), Some("do it"));

        let empty = serde_json::json!({"type": "UserMessage", "text": "  "});
        assert_eq!(user_message(&empty), None);
    }

    #[test]
    fn a_wrapped_item_is_read_through_its_single_key() {
        let wrapped = serde_json::json!({"UserMessage": {"text": "wrapped"}});
        assert_eq!(item_type(&wrapped), Some("UserMessage"));
        assert_eq!(user_message(&wrapped).as_deref(), Some("wrapped"));
    }
}
