//! The event model, mirroring `schema/event.schema.json`.
//!
//! Every recorded fact is an [`Event`]. Events are chained: each one hashes
//! the previous hash together with its own timestamp, kind and canonical
//! payload, so a single edited row breaks everything after it.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// `prev_hash` of the first event in a chain: 64 zeros.
pub const GENESIS_PREV_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// At most this many lines of diff are kept per file.
pub const MAX_DIFF_EXCERPT_LINES: usize = 20;

// ---------------------------------------------------------------------------
// kinds
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Intent,
    Commit,
    AiSession,
    Decision,
    Pattern,
    Summary,
    Hide,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::Intent => "intent",
            EventKind::Commit => "commit",
            EventKind::AiSession => "ai_session",
            EventKind::Decision => "decision",
            EventKind::Pattern => "pattern",
            EventKind::Summary => "summary",
            EventKind::Hide => "hide",
        }
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown event kind `{0}`")]
pub struct UnknownEventKind(pub String);

impl FromStr for EventKind {
    type Err = UnknownEventKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "intent" => EventKind::Intent,
            "commit" => EventKind::Commit,
            "ai_session" => EventKind::AiSession,
            "decision" => EventKind::Decision,
            "pattern" => EventKind::Pattern,
            "summary" => EventKind::Summary,
            "hide" => EventKind::Hide,
            other => return Err(UnknownEventKind(other.to_string())),
        })
    }
}

// ---------------------------------------------------------------------------
// payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentPayload {
    pub what: String,
    pub for_whom: String,
    pub tools: Vec<String>,
    pub record_ai_sessions: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl FileStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileStatus::Added => "added",
            FileStatus::Modified => "modified",
            FileStatus::Deleted => "deleted",
            FileStatus::Renamed => "renamed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitFile {
    pub path: String,
    pub added: u64,
    pub removed: u64,
    pub status: FileStatus,
    /// At most [`MAX_DIFF_EXCERPT_LINES`] lines. Absent for binary, ignored or
    /// redacted files — the schema has no null here, so it is omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_excerpt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitPayload {
    pub sha: String,
    pub message: String,
    pub author: String,
    pub parents: Vec<String>,
    pub files: Vec<CommitFile>,
    pub secs_since_prev: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiSessionFile {
    pub path: String,
    pub ai_lines: u64,
    pub ai_lines_modified_by_human: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiSessionPayload {
    /// `claude-code | cursor | codex | copilot | windsurf | other`
    pub tool: String,
    pub model: Option<String>,
    pub session_id: String,
    /// `None` when redacted or unavailable.
    pub prompt: Option<String>,
    pub response_excerpt: Option<String>,
    pub files: Vec<AiSessionFile>,
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionPayload {
    pub text: String,
    /// Private decisions are chained but never rendered.
    pub private: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatternPayload {
    pub pattern: PatternKind,
    pub commits: Vec<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternKind {
    Revert,
    Retry,
    FixAfterAi,
    Stall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryPayload {
    pub week_start: String,
    pub week_end: String,
    pub text: String,
    /// `ollama:<model>` | `remote:<model>` | `heuristic` | `user`
    pub generated_by: String,
    pub edited: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HidePayload {
    /// Only `ai_session` is hideable in v1.
    pub target_kind: String,
    pub target_id: String,
}

/// A typed payload plus the kind it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    Intent(IntentPayload),
    Commit(CommitPayload),
    AiSession(AiSessionPayload),
    Decision(DecisionPayload),
    Pattern(PatternPayload),
    Summary(SummaryPayload),
    Hide(HidePayload),
}

impl Payload {
    pub fn kind(&self) -> EventKind {
        match self {
            Payload::Intent(_) => EventKind::Intent,
            Payload::Commit(_) => EventKind::Commit,
            Payload::AiSession(_) => EventKind::AiSession,
            Payload::Decision(_) => EventKind::Decision,
            Payload::Pattern(_) => EventKind::Pattern,
            Payload::Summary(_) => EventKind::Summary,
            Payload::Hide(_) => EventKind::Hide,
        }
    }

    /// Infallible: every payload struct is a plain object of JSON scalars.
    pub fn to_value(&self) -> serde_json::Value {
        let v = match self {
            Payload::Intent(p) => serde_json::to_value(p),
            Payload::Commit(p) => serde_json::to_value(p),
            Payload::AiSession(p) => serde_json::to_value(p),
            Payload::Decision(p) => serde_json::to_value(p),
            Payload::Pattern(p) => serde_json::to_value(p),
            Payload::Summary(p) => serde_json::to_value(p),
            Payload::Hide(p) => serde_json::to_value(p),
        };
        v.expect("payload structs always serialise")
    }
}

// ---------------------------------------------------------------------------
// the event
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub seq: i64,
    pub id: String,
    pub ts: String,
    pub kind: EventKind,
    pub commit_sha: Option<String>,
    pub payload: serde_json::Value,
    pub prev_hash: String,
    pub hash: String,
}

impl Event {
    /// The hash this event should carry, recomputed from its own contents.
    pub fn expected_hash(&self) -> String {
        hash_for(&self.prev_hash, &self.ts, self.kind, &self.payload)
    }

    pub fn payload_as<T: serde::de::DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_value(self.payload.clone())
    }

    /// One line describing the event, for `pop log`.
    pub fn title(&self) -> String {
        match self.kind {
            EventKind::Intent => self
                .payload
                .get("what")
                .and_then(|v| v.as_str())
                .unwrap_or("intent recorded")
                .to_string(),
            EventKind::Commit => first_line(
                self.payload
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            ),
            EventKind::AiSession => {
                let tool = self
                    .payload
                    .get("tool")
                    .and_then(|v| v.as_str())
                    .unwrap_or("ai");
                let prompt = self
                    .payload
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(prompt not recorded)");
                format!("{tool}: {}", first_line(prompt))
            }
            EventKind::Decision => {
                let private = self
                    .payload
                    .get("private")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let text = self
                    .payload
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if private {
                    format!("{text} (private)")
                } else {
                    text.to_string()
                }
            }
            EventKind::Pattern => {
                let p = self
                    .payload
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("pattern");
                let detail = self
                    .payload
                    .get("detail")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                format!("{p}: {detail}")
            }
            EventKind::Summary => {
                let start = self
                    .payload
                    .get("week_start")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                format!("week of {start}")
            }
            EventKind::Hide => {
                let target = self
                    .payload
                    .get("target_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                format!("hid {target}")
            }
        }
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

// ---------------------------------------------------------------------------
// canonical JSON and hashing
// ---------------------------------------------------------------------------

/// Canonical JSON: object keys sorted, no whitespace.
///
/// Written by hand rather than leaning on `serde_json`'s map ordering, so that
/// enabling `preserve_order` anywhere in the dependency graph cannot silently
/// change every hash we have ever written.
pub fn canonical_json(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&escape_string(k));
                out.push(':');
                write_canonical(&map[*k], out);
            }
            out.push('}');
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn escape_string(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// `sha256(prev_hash + "\n" + ts + "\n" + kind + "\n" + canonical_json(payload))`,
/// hex, lowercase.
pub fn hash_for(
    prev_hash: &str,
    ts: &str,
    kind: EventKind,
    payload: &serde_json::Value,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(b"\n");
    hasher.update(ts.as_bytes());
    hasher.update(b"\n");
    hasher.update(kind.as_str().as_bytes());
    hasher.update(b"\n");
    hasher.update(canonical_json(payload).as_bytes());
    hex::encode(hasher.finalize())
}

/// RFC 3339 UTC, second resolution — what goes in `ts`.
pub fn now_ts() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// A fresh uuid v7 (time-ordered, so ids sort like the chain).
pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_is_sorted_and_tight() {
        let v = serde_json::json!({"b": 1, "a": {"d": 2, "c": 3}});
        assert_eq!(canonical_json(&v), r#"{"a":{"c":3,"d":2},"b":1}"#);
    }

    #[test]
    fn canonical_json_escapes_keys_and_values() {
        let v = serde_json::json!({"a\"b": "line\nbreak"});
        assert_eq!(canonical_json(&v), r#"{"a\"b":"line\nbreak"}"#);
    }

    #[test]
    fn diff_excerpt_is_omitted_rather_than_nulled() {
        let f = CommitFile {
            path: "a.rs".into(),
            added: 1,
            removed: 0,
            status: FileStatus::Added,
            diff_excerpt: None,
        };
        let s = canonical_json(&serde_json::to_value(f).unwrap());
        assert!(!s.contains("diff_excerpt"), "{s}");
    }

    #[test]
    fn optional_ai_session_fields_are_nulled_rather_than_omitted() {
        let p = Payload::AiSession(AiSessionPayload {
            tool: "claude-code".into(),
            model: None,
            session_id: "s".into(),
            prompt: None,
            response_excerpt: None,
            files: vec![],
            accepted: false,
        });
        let s = canonical_json(&p.to_value());
        assert!(s.contains(r#""model":null"#), "{s}");
        assert!(s.contains(r#""prompt":null"#), "{s}");
    }

    #[test]
    fn now_ts_round_trips_as_rfc3339() {
        let ts = now_ts();
        chrono::DateTime::parse_from_rfc3339(&ts).unwrap();
        assert!(ts.ends_with('Z'), "{ts}");
    }
}
