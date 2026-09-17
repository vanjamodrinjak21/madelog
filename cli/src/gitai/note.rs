//! The Git AI note format.
//!
//! A note is a text blob: attestation lines, a line that is exactly `---`,
//! then one JSON object. The attestations say which lines of which file each
//! key wrote; the JSON says what each key is. See
//! `docs/superpowers/specs/gitai-format.md`.
//!
//! Nothing here touches git or the filesystem, so every rule is testable
//! against the fixtures in `cli/tests/fixtures/gitai/`.

use std::collections::BTreeMap;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

/// The separator between the attestations and the JSON metadata.
const SEPARATOR: &str = "---";

// ---------------------------------------------------------------------------
// attestations
// ---------------------------------------------------------------------------

/// An inclusive, 1-indexed span of lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

impl LineRange {
    pub fn line_count(&self) -> u64 {
        u64::from(self.end - self.start) + 1
    }
}

/// One `<key> <ranges>` line under a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Exactly as written, including any `::t_<hex>` trace suffix.
    pub key: String,
    pub ranges: Vec<LineRange>,
}

/// One file and the entries attributed to it, in the order they were written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAttestation {
    pub path: String,
    pub entries: Vec<Entry>,
}

impl FileAttestation {
    /// Lines each entry still owns once later entries have taken theirs,
    /// indexed like [`FileAttestation::entries`].
    ///
    /// Git AI writes overlapping spans when a line was rewritten inside one
    /// commit; the last word wins.
    pub fn owned_lines(&self) -> Vec<u64> {
        let mut owned = vec![0u64; self.entries.len()];
        // Walking backwards means "already claimed" is exactly "claimed by a
        // later entry", which is the rule we need.
        let mut claimed: Vec<LineRange> = Vec::new();
        for (idx, entry) in self.entries.iter().enumerate().rev() {
            for range in &entry.ranges {
                owned[idx] += unclaimed_lines(*range, &claimed);
                claim(&mut claimed, *range);
            }
        }
        owned
    }
}

/// How many lines of `range` no interval in `claimed` covers.
fn unclaimed_lines(range: LineRange, claimed: &[LineRange]) -> u64 {
    let mut count = range.line_count();
    for c in claimed {
        if c.end < range.start {
            continue;
        }
        if c.start > range.end {
            break;
        }
        let lo = c.start.max(range.start);
        let hi = c.end.min(range.end);
        count -= u64::from(hi - lo) + 1;
    }
    count
}

/// Add `range` to a sorted, disjoint interval list.
fn claim(claimed: &mut Vec<LineRange>, range: LineRange) {
    claimed.push(range);
    claimed.sort_unstable_by_key(|r| r.start);
    let mut merged: Vec<LineRange> = Vec::with_capacity(claimed.len());
    for r in claimed.drain(..) {
        match merged.last_mut() {
            // Touching intervals merge too: it only makes the list shorter.
            Some(last) if r.start <= last.end.saturating_add(1) => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    *claimed = merged;
}

// ---------------------------------------------------------------------------
// key routing
// ---------------------------------------------------------------------------

/// Which map of the JSON metadata a key belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// `s_<hex>`, optionally followed by `::t_<hex>`.
    Session,
    /// `h_<hex>`.
    Human,
    /// No prefix: a legacy `prompts` entry.
    Prompt,
}

/// The map a key belongs to and the key to look up there.
///
/// Id lengths are never assumed; only the prefix decides.
pub fn route(key: &str) -> (Route, &str) {
    if key.starts_with("s_") {
        (Route::Session, key.split("::").next().unwrap_or(key))
    } else if key.starts_with("h_") {
        (Route::Human, key)
    } else {
        (Route::Prompt, key)
    }
}

// ---------------------------------------------------------------------------
// JSON metadata
// ---------------------------------------------------------------------------

/// `{tool, id, model}`. Every field is optional: Git AI nulls what it does
/// not know, and adds fields we have not seen yet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct AgentId {
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct SessionRecord {
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    #[serde(default)]
    pub human_author: Option<String>,
}

/// The pre-3.0 shape. `overriden_lines` keeps Git AI's spelling.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct PromptRecord {
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    #[serde(default)]
    pub human_author: Option<String>,
    #[serde(default)]
    pub accepted_lines: u64,
    #[serde(default)]
    pub overriden_lines: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct HumanRecord {
    #[serde(default)]
    pub author: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Meta {
    schema_version: String,
    #[serde(default)]
    git_ai_version: Option<String>,
    #[serde(default)]
    base_commit_sha: Option<String>,
    #[serde(default)]
    prompts: BTreeMap<String, PromptRecord>,
    #[serde(default)]
    humans: BTreeMap<String, HumanRecord>,
    #[serde(default)]
    sessions: BTreeMap<String, SessionRecord>,
}

// ---------------------------------------------------------------------------
// the parsed note
// ---------------------------------------------------------------------------

/// Lines of one file attributed to one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLines {
    pub path: String,
    pub lines: u64,
}

/// Everything one key wrote in this commit, across files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    pub route: Route,
    /// The routed key: `s_<hex>` without its trace suffix.
    pub key: String,
    /// Sorted by path, so the same note always maps to the same event.
    pub files: Vec<FileLines>,
    pub total: u64,
}

/// A parsed Git AI note.
#[derive(Debug, Clone)]
pub struct Note {
    pub schema_version: String,
    pub git_ai_version: Option<String>,
    pub base_commit_sha: Option<String>,
    pub prompts: BTreeMap<String, PromptRecord>,
    pub humans: BTreeMap<String, HumanRecord>,
    pub sessions: BTreeMap<String, SessionRecord>,
    /// In note order.
    pub files: Vec<FileAttestation>,
}

impl Note {
    /// Every key that still owns at least one line, in the order it first
    /// appears in the attestations.
    pub fn attribution(&self) -> Vec<Attribution> {
        let mut order: Vec<String> = Vec::new();
        let mut routes: BTreeMap<String, Route> = BTreeMap::new();
        let mut per_key: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();

        for file in &self.files {
            let owned = file.owned_lines();
            for (entry, lines) in file.entries.iter().zip(owned) {
                if lines == 0 {
                    continue;
                }
                let (route, key) = route(&entry.key);
                if !per_key.contains_key(key) {
                    order.push(key.to_string());
                    routes.insert(key.to_string(), route);
                }
                *per_key
                    .entry(key.to_string())
                    .or_default()
                    .entry(file.path.clone())
                    .or_default() += lines;
            }
        }

        order
            .into_iter()
            .map(|key| {
                let files: Vec<FileLines> = per_key[&key]
                    .iter()
                    .map(|(path, lines)| FileLines {
                        path: path.clone(),
                        lines: *lines,
                    })
                    .collect();
                Attribution {
                    route: routes[&key],
                    total: files.iter().map(|f| f.lines).sum(),
                    key,
                    files,
                }
            })
            .collect()
    }

    /// Does the JSON describe this key? A foreign cherry-pick brings
    /// attestations whose keys were never in our note.
    pub fn knows(&self, attribution: &Attribution) -> bool {
        match attribution.route {
            Route::Session => self.sessions.contains_key(&attribution.key),
            Route::Human => self.humans.contains_key(&attribution.key),
            Route::Prompt => self.prompts.contains_key(&attribution.key),
        }
    }

    /// Keys that attribute lines but appear in no map. Counted, never fatal.
    pub fn unknown_keys(&self) -> Vec<String> {
        self.attribution()
            .into_iter()
            .filter(|a| !self.knows(a))
            .map(|a| a.key)
            .collect()
    }

    /// Lines attributed to keys this note does not describe.
    pub fn unknown_lines(&self) -> u64 {
        self.attribution()
            .iter()
            .filter(|a| !self.knows(a))
            .map(|a| a.total)
            .sum()
    }
}

// ---------------------------------------------------------------------------
// parsing
// ---------------------------------------------------------------------------

/// Parse a note blob.
///
/// Errors describe a note pop cannot read at all. A key it cannot resolve is
/// not an error — see [`Note::unknown_keys`].
pub fn parse(blob: &str) -> Result<Note> {
    let (attestations, json) = split(blob)?;
    let files = parse_attestations(attestations)?;
    let meta: Meta = serde_json::from_str(json.trim())
        .context("the JSON metadata of the Git AI note is not readable")?;
    Ok(Note {
        schema_version: meta.schema_version,
        git_ai_version: meta.git_ai_version,
        base_commit_sha: meta.base_commit_sha,
        prompts: meta.prompts,
        humans: meta.humans,
        sessions: meta.sessions,
        files,
    })
}

/// Split at the **first** line that is exactly `---`. A later `---` may well
/// be part of the JSON, or of a path.
fn split(blob: &str) -> Result<(&str, &str)> {
    let mut offset = 0usize;
    for raw in blob.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line == SEPARATOR {
            return Ok((&blob[..offset], &blob[offset + raw.len()..]));
        }
        offset += raw.len();
    }
    bail!("the Git AI note has no `{SEPARATOR}` line between attestations and metadata")
}

fn parse_attestations(text: &str) -> Result<Vec<FileAttestation>> {
    let mut files: Vec<FileAttestation> = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let lineno = idx + 1;
        if line.trim().is_empty() {
            continue;
        }
        match line.strip_prefix("  ") {
            Some(rest) => {
                let file = files.last_mut().ok_or_else(|| {
                    anyhow!("line {lineno} of the Git AI note attributes lines before any file")
                })?;
                file.entries.push(parse_entry(rest, lineno)?);
            }
            None => files.push(FileAttestation {
                path: parse_path(line, lineno)?,
                entries: Vec::new(),
            }),
        }
    }
    Ok(files)
}

/// A path at column 0, quoted with `"` when it contains whitespace.
fn parse_path(line: &str, lineno: usize) -> Result<String> {
    let line = line.trim_end();
    let Some(inner) = line.strip_prefix('"') else {
        return Ok(line.to_string());
    };
    let inner = inner.strip_suffix('"').ok_or_else(|| {
        anyhow!("line {lineno} of the Git AI note has an unterminated quoted path")
    })?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => bail!("line {lineno} of the Git AI note ends in a lone backslash"),
        }
    }
    Ok(out)
}

fn parse_entry(rest: &str, lineno: usize) -> Result<Entry> {
    let mut parts = rest.split_whitespace();
    let key = parts
        .next()
        .ok_or_else(|| anyhow!("line {lineno} of the Git AI note has no key"))?;
    let ranges = parts.next().ok_or_else(|| {
        anyhow!("line {lineno} of the Git AI note has the key `{key}` but no line ranges")
    })?;
    Ok(Entry {
        key: key.to_string(),
        ranges: parse_ranges(ranges, lineno)?,
    })
}

/// `1-4,9,12` — 1-indexed, inclusive, no spaces.
fn parse_ranges(text: &str, lineno: usize) -> Result<Vec<LineRange>> {
    let mut out = Vec::new();
    for token in text.split(',') {
        if token.is_empty() {
            continue;
        }
        let range = match token.split_once('-') {
            Some((start, end)) => LineRange {
                start: number(start, token, lineno)?,
                end: number(end, token, lineno)?,
            },
            None => {
                let n = number(token, token, lineno)?;
                LineRange { start: n, end: n }
            }
        };
        if range.start == 0 || range.end < range.start {
            bail!("line {lineno} of the Git AI note has the impossible range `{token}`");
        }
        out.push(range);
    }
    if out.is_empty() {
        bail!("line {lineno} of the Git AI note has an empty range list");
    }
    Ok(out)
}

fn number(part: &str, token: &str, lineno: usize) -> Result<u32> {
    part.parse::<u32>()
        .map_err(|_| anyhow!("line {lineno} of the Git AI note has the unreadable range `{token}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(body: &str) -> Note {
        parse(body).unwrap()
    }

    #[test]
    fn an_empty_attestation_section_is_fine() {
        let n = note("---\n{\"schema_version\":\"authorship/3.0.0\"}");
        assert!(n.files.is_empty());
        assert!(n.attribution().is_empty());
        assert!(n.sessions.is_empty());
    }

    #[test]
    fn crlf_notes_parse_like_lf_notes() {
        let n = note("a.rs\r\n  s_a 1-2\r\n---\r\n{\"schema_version\":\"authorship/3.0.0\"}");
        assert_eq!(n.files[0].path, "a.rs");
        assert_eq!(n.files[0].entries[0].ranges[0].line_count(), 2);
    }

    #[test]
    fn a_single_line_range_counts_as_one() {
        let ranges = parse_ranges("7", 1).unwrap();
        assert_eq!(ranges, [LineRange { start: 7, end: 7 }]);
        assert_eq!(ranges[0].line_count(), 1);
    }

    #[test]
    fn line_zero_is_rejected() {
        assert!(parse_ranges("0-3", 1).is_err());
    }

    #[test]
    fn ranges_that_overlap_inside_one_entry_are_not_counted_twice() {
        let file = FileAttestation {
            path: "a.rs".into(),
            entries: vec![Entry {
                key: "s_a".into(),
                ranges: vec![
                    LineRange { start: 1, end: 5 },
                    LineRange { start: 3, end: 8 },
                ],
            }],
        };
        assert_eq!(file.owned_lines(), [8]);
    }

    #[test]
    fn an_entry_fully_covered_by_a_later_one_owns_nothing() {
        let file = FileAttestation {
            path: "a.rs".into(),
            entries: vec![
                Entry {
                    key: "s_a".into(),
                    ranges: vec![LineRange { start: 1, end: 4 }],
                },
                Entry {
                    key: "s_b".into(),
                    ranges: vec![LineRange { start: 1, end: 9 }],
                },
            ],
        };
        assert_eq!(file.owned_lines(), [0, 9]);
    }

    #[test]
    fn an_entry_before_any_file_is_an_error() {
        assert!(parse("  s_a 1-2\n---\n{\"schema_version\":\"v\"}").is_err());
    }

    #[test]
    fn a_key_without_ranges_is_an_error() {
        let err = parse("a.rs\n  s_a\n---\n{\"schema_version\":\"v\"}").unwrap_err();
        assert!(format!("{err:#}").contains("no line ranges"), "{err:#}");
    }

    #[test]
    fn a_missing_schema_version_is_an_error() {
        let err = parse("---\n{}").unwrap_err();
        assert!(format!("{err:#}").contains("Git AI note"), "{err:#}");
    }

    #[test]
    fn the_first_separator_wins() {
        let n = note("a.rs\n  s_a 1\n---\n{\"schema_version\":\"v\",\"base_commit_sha\":\"---\"}");
        assert_eq!(n.base_commit_sha.as_deref(), Some("---"));
        assert_eq!(n.files.len(), 1);
    }

    #[test]
    fn a_quoted_path_keeps_its_escapes() {
        assert_eq!(parse_path(r#""a b\"c\\d.md""#, 1).unwrap(), "a b\"c\\d.md");
        assert_eq!(parse_path("plain path.md", 1).unwrap(), "plain path.md");
    }

    #[test]
    fn unknown_json_fields_are_tolerated() {
        let n = note(
            r#"a.rs
  s_a 1-2
---
{"schema_version":"authorship/4.0.0","tomorrow":{"x":1},
 "sessions":{"s_a":{"agent_id":{"tool":"claude","id":"x","model":"m","future":1},"later":true}}}"#,
        );
        assert_eq!(n.schema_version, "authorship/4.0.0");
        assert_eq!(
            n.sessions["s_a"].agent_id.as_ref().unwrap().tool.as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn a_null_agent_id_does_not_break_the_record() {
        let n = note(
            r#"a.rs
  s_a 1
---
{"schema_version":"v","sessions":{"s_a":{"agent_id":null,"human_author":null}}}"#,
        );
        assert_eq!(n.sessions["s_a"].agent_id, None);
        assert!(n.knows(&n.attribution()[0]));
    }
}
