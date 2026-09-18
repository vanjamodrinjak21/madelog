//! Pattern detection over the recorded log (spec §4.2).
//!
//! Four rules run over the ordered `commit` and `ai_session` events and
//! produce [`PatternPayload`]s. Everything here is pure and deterministic:
//! the same events always give the same patterns in the same order, which is
//! what lets `madelog analyze` run twice without recording anything new.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};

use crate::event::{
    AiSessionPayload, CommitFile, CommitPayload, Event, EventKind, PatternKind, PatternPayload,
};

// ---------------------------------------------------------------------------
// thresholds — spec §4.2
// ---------------------------------------------------------------------------

/// A revert has to undo something recent to be worth naming.
///
/// The spec says "a recent commit"; two days is our reading of recent.
pub const REVERT_WINDOW_SECS: i64 = 48 * 3600;
/// "three or more consecutive commits"
pub const RETRY_MIN_COMMITS: usize = 3;
/// "within 2 hours"
pub const RETRY_WINDOW_SECS: i64 = 2 * 3600;
/// "messages sharing 60 % of tokens" — Jaccard overlap against the first
/// message of the run.
pub const RETRY_TOKEN_OVERLAP: f64 = 0.60;
/// "within 24 h after a commit with AI-attributed lines"
pub const FIX_AFTER_AI_WINDOW_SECS: i64 = 24 * 3600;
/// "gap of 5+ days between commits"
pub const STALL_GAP_SECS: i64 = 5 * 86_400;
/// "followed by a commit touching 10+ files or 300+ lines"
pub const STALL_MIN_FILES: usize = 10;
pub const STALL_MIN_LINES: u64 = 300;

/// Words that mark a commit as a fix, per spec §4.2.
const FIX_WORDS: [&str; 6] = ["fix", "bug", "wrong", "broke", "revert", "oops"];

// ---------------------------------------------------------------------------
// the view the rules work on
// ---------------------------------------------------------------------------

/// One `commit` event plus the AI sessions anchored to the same commit.
#[derive(Debug, Clone)]
pub struct Commit {
    /// The id of the `commit` event, for moment references.
    pub event_id: String,
    pub sha: String,
    pub ts: DateTime<Utc>,
    pub message: String,
    pub files: Vec<CommitFile>,
    pub ai: Vec<AiSessionPayload>,
    /// The ids of the `ai_session` events anchored to this commit.
    pub ai_event_ids: Vec<String>,
}

impl Commit {
    /// The seven characters a human reads a sha by.
    pub fn short(&self) -> &str {
        &self.sha[..7.min(self.sha.len())]
    }

    /// The commit's subject line, trimmed.
    pub fn subject(&self) -> &str {
        self.message.lines().next().unwrap_or("").trim()
    }

    pub fn paths(&self) -> BTreeSet<&str> {
        self.files.iter().map(|f| f.path.as_str()).collect()
    }

    pub fn lines_changed(&self) -> u64 {
        self.files.iter().map(|f| f.added + f.removed).sum()
    }

    /// Lines the AI proposed in this commit, across every session.
    pub fn ai_lines(&self) -> u64 {
        self.ai
            .iter()
            .flat_map(|s| s.files.iter())
            .map(|f| f.ai_lines)
            .sum()
    }

    /// AI lines a human went back and changed.
    pub fn ai_lines_reworked(&self) -> u64 {
        self.ai
            .iter()
            .flat_map(|s| s.files.iter())
            .map(|f| f.ai_lines_modified_by_human)
            .sum()
    }

    pub fn ai_paths(&self) -> BTreeSet<&str> {
        self.ai
            .iter()
            .flat_map(|s| s.files.iter())
            .filter(|f| f.ai_lines > 0)
            .map(|f| f.path.as_str())
            .collect()
    }
}

/// The ordered commits of a log, each carrying its AI sessions.
///
/// Events whose timestamp or payload cannot be read are skipped rather than
/// failing the whole analysis — madelog never refuses to look at a log.
pub fn commits(events: &[Event]) -> Vec<Commit> {
    let mut out: Vec<Commit> = Vec::new();
    for event in events {
        if event.kind != EventKind::Commit {
            continue;
        }
        let (Ok(payload), Some(ts)) = (event.payload_as::<CommitPayload>(), parse_ts(&event.ts))
        else {
            continue;
        };
        out.push(Commit {
            event_id: event.id.clone(),
            sha: payload.sha,
            ts,
            message: payload.message,
            files: payload.files,
            ai: Vec::new(),
            ai_event_ids: Vec::new(),
        });
    }
    for event in events {
        if event.kind != EventKind::AiSession {
            continue;
        }
        let (Some(sha), Ok(payload)) = (
            event.commit_sha.as_deref(),
            event.payload_as::<AiSessionPayload>(),
        ) else {
            continue;
        };
        if let Some(commit) = out.iter_mut().find(|c| c.sha == sha) {
            commit.ai.push(payload);
            commit.ai_event_ids.push(event.id.clone());
        }
    }
    out
}

fn parse_ts(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

// ---------------------------------------------------------------------------
// detection
// ---------------------------------------------------------------------------

/// Every pattern the four rules find, in the order the commits happened.
pub fn detect(events: &[Event]) -> Vec<PatternPayload> {
    let commits = commits(events);
    let mut found: Vec<(usize, u8, PatternPayload)> = Vec::new();
    found.extend(reverts(&commits));
    found.extend(retries(&commits));
    found.extend(fixes_after_ai(&commits));
    found.extend(stalls(&commits));
    found.sort_by_key(|(at, rank, _)| (*at, *rank));
    found.into_iter().map(|(_, _, p)| p).collect()
}

/// The name a pattern is written and printed under.
pub fn kind_str(kind: PatternKind) -> &'static str {
    match kind {
        PatternKind::Revert => "revert",
        PatternKind::Retry => "retry",
        PatternKind::FixAfterAi => "fix_after_ai",
        PatternKind::Stall => "stall",
    }
}

/// `revert` — the message says so, or the file changes undo an earlier commit.
fn reverts(commits: &[Commit]) -> Vec<(usize, u8, PatternPayload)> {
    let mut out = Vec::new();
    for (i, c) in commits.iter().enumerate() {
        let says_revert = c.subject().to_lowercase().starts_with("revert");
        let undone = undone_commit(commits, i);
        if !says_revert && undone.is_none() {
            continue;
        }
        let payload = match undone {
            Some(j) => {
                let prior = &commits[j];
                let gap = (c.ts - prior.ts).num_seconds();
                PatternPayload {
                    pattern: PatternKind::Revert,
                    commits: vec![prior.sha.clone(), c.sha.clone()],
                    detail: format!(
                        "Reverted {} changed in {} {} earlier.",
                        files_phrase(prior.files.len()),
                        prior.short(),
                        humanize_gap(gap)
                    ),
                }
            }
            None => PatternPayload {
                pattern: PatternKind::Revert,
                commits: vec![c.sha.clone()],
                detail: format!(
                    "{} undoes earlier work in {}.",
                    c.short(),
                    files_phrase(c.files.len())
                ),
            },
        };
        out.push((i, 0, payload));
    }
    out
}

/// The most recent commit whose changes `commits[i]` is the inverse of.
///
/// At least one file has to be lopsided: two commits that each change the
/// same three lines of the same file are the inverse of one another on paper,
/// and are just ordinary work in practice.
fn undone_commit(commits: &[Commit], i: usize) -> Option<usize> {
    let c = &commits[i];
    if c.files.is_empty() || c.lines_changed() == 0 || c.files.iter().all(|f| f.added == f.removed)
    {
        return None;
    }
    (0..i).rev().find(|&j| {
        let prior = &commits[j];
        (c.ts - prior.ts).num_seconds() <= REVERT_WINDOW_SECS
            && prior.paths() == c.paths()
            && c.files.iter().all(|f| {
                prior
                    .files
                    .iter()
                    .find(|p| p.path == f.path)
                    .is_some_and(|p| p.added == f.removed && p.removed == f.added)
            })
    })
}

/// `retry` — three or more consecutive commits inside two hours, same files,
/// messages sharing 60 % of their tokens.
fn retries(commits: &[Commit]) -> Vec<(usize, u8, PatternPayload)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + RETRY_MIN_COMMITS <= commits.len() {
        let first = &commits[i];
        let paths = first.paths();
        let tokens = tokens_of(first.subject());
        let mut j = i + 1;
        while j < commits.len()
            && commits[j].paths() == paths
            && !paths.is_empty()
            && (commits[j].ts - first.ts).num_seconds() <= RETRY_WINDOW_SECS
            && overlap(&tokens, &tokens_of(commits[j].subject())) >= RETRY_TOKEN_OVERLAP
        {
            j += 1;
        }
        let run = j - i;
        if run >= RETRY_MIN_COMMITS {
            let span = (commits[j - 1].ts - first.ts).num_seconds();
            out.push((
                j - 1,
                1,
                PatternPayload {
                    pattern: PatternKind::Retry,
                    commits: commits[i..j].iter().map(|c| c.sha.clone()).collect(),
                    detail: format!(
                        "{} commits in {} reworked {}.",
                        capitalize(&number_word(run as i64)),
                        humanize_gap(span),
                        match paths.len() {
                            1 => "the same file".to_string(),
                            n => format!("the same {n} files"),
                        }
                    ),
                },
            ));
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// `fix_after_ai` — a fix, within a day, to the files the AI just wrote.
fn fixes_after_ai(commits: &[Commit]) -> Vec<(usize, u8, PatternPayload)> {
    let mut out = Vec::new();
    for (i, c) in commits.iter().enumerate() {
        let looks_like_a_fix = mentions_fix(c.subject()) || c.ai_lines_reworked() > 0;
        if !looks_like_a_fix {
            continue;
        }
        let touched = c.paths();
        let earlier = (0..i).rev().find(|&j| {
            let prior = &commits[j];
            prior.ai_lines() > 0
                && (c.ts - prior.ts).num_seconds() <= FIX_AFTER_AI_WINDOW_SECS
                && prior.ai_paths().iter().any(|p| touched.contains(p))
        });
        let Some(j) = earlier else { continue };
        let prior = &commits[j];
        let shared: Vec<&str> = prior
            .ai_paths()
            .into_iter()
            .filter(|p| touched.contains(p))
            .collect();
        out.push((
            i,
            2,
            PatternPayload {
                pattern: PatternKind::FixAfterAi,
                commits: vec![prior.sha.clone(), c.sha.clone()],
                detail: format!(
                    "{} reworked AI-written code in {} {} after {}.",
                    c.short(),
                    path_phrase(&shared),
                    humanize_gap((c.ts - prior.ts).num_seconds()),
                    prior.short()
                ),
            },
        ));
    }
    out
}

/// `stall` — five quiet days, then a large commit.
fn stalls(commits: &[Commit]) -> Vec<(usize, u8, PatternPayload)> {
    let mut out = Vec::new();
    for i in 1..commits.len() {
        let c = &commits[i];
        let gap = (c.ts - commits[i - 1].ts).num_seconds();
        let big = c.files.len() >= STALL_MIN_FILES || c.lines_changed() >= STALL_MIN_LINES;
        if gap < STALL_GAP_SECS || !big {
            continue;
        }
        out.push((
            i,
            3,
            PatternPayload {
                pattern: PatternKind::Stall,
                commits: vec![commits[i - 1].sha.clone(), c.sha.clone()],
                detail: format!(
                    "After {} without commits, {} changed {} ({} lines).",
                    humanize_gap(gap),
                    c.short(),
                    files_phrase(c.files.len()),
                    c.lines_changed()
                ),
            },
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// text helpers, shared with `moments`
// ---------------------------------------------------------------------------

/// Does this message read like a fix? Spec §4.2 lists the words.
///
/// Matching is on whole words, so "prefix" is not a fix, but inflections
/// ("fixed", "bugs", "broken", "reverted") are.
pub(crate) fn mentions_fix(message: &str) -> bool {
    message
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| FIX_WORDS.iter().any(|fix| word.starts_with(fix)))
}

fn tokens_of(message: &str) -> BTreeSet<String> {
    message
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Jaccard overlap of two token sets; 1.0 for two empty messages.
fn overlap(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(b).count() as f64 / union as f64
}

/// "1 file" / "4 files".
pub(crate) fn files_phrase(n: usize) -> String {
    if n == 1 {
        "1 file".to_string()
    } else {
        format!("{n} files")
    }
}

/// The paths themselves while they are few, a count once they are many.
pub(crate) fn path_phrase(paths: &[&str]) -> String {
    match paths.len() {
        0 => "the same files".to_string(),
        1 => paths[0].to_string(),
        2 => format!("{} and {}", paths[0], paths[1]),
        n => format!("{} and {} other files", paths[0], n - 1),
    }
}

/// A gap as a person would say it: "two hours", "six days", "47 minutes".
pub(crate) fn humanize_gap(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        return "less than a minute".to_string();
    }
    let (n, unit) = if secs < 3600 {
        (secs / 60, "minute")
    } else if secs < 48 * 3600 {
        (secs / 3600, "hour")
    } else {
        (secs / 86_400, "day")
    };
    let plural = if n == 1 { "" } else { "s" };
    format!("{} {unit}{plural}", number_word(n))
}

/// Small numbers read better as words.
pub(crate) fn number_word(n: i64) -> String {
    const WORDS: [&str; 13] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve",
    ];
    match usize::try_from(n) {
        Ok(i) if i < WORDS.len() => WORDS[i].to_string(),
        _ => n.to_string(),
    }
}

pub(crate) fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// A commit message fit to quote inside a sentence: one line, no trailing
/// punctuation, never longer than `max`.
pub(crate) fn quotable(message: &str, max: usize) -> String {
    let line = message.lines().next().unwrap_or("").trim();
    let line = line.trim_end_matches(['.', '!', '?', ' ']);
    if line.chars().count() <= max {
        return line.to_string();
    }
    let cut: String = line.chars().take(max).collect();
    let cut = cut.trim_end();
    format!("{cut}...")
}

#[cfg(test)]
pub(crate) mod testdata {
    //! Synthetic events, so the rules can be tested without a git repository.

    use crate::event::{
        AiSessionFile, AiSessionPayload, CommitFile, CommitPayload, Event, EventKind, FileStatus,
    };

    /// A 40-character sha that still reads as its prefix.
    pub fn sha(prefix: &str) -> String {
        let mut s = prefix.to_string();
        while s.len() < 40 {
            s.push('0');
        }
        s
    }

    /// `2026-09-01T00:00:00Z` plus `secs`.
    pub fn at(secs: i64) -> String {
        let base = chrono::DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z").unwrap();
        (base + chrono::Duration::seconds(secs))
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    }

    pub fn file(path: &str, added: u64, removed: u64) -> CommitFile {
        CommitFile {
            path: path.to_string(),
            added,
            removed,
            status: if removed == 0 {
                FileStatus::Added
            } else {
                FileStatus::Modified
            },
            diff_excerpt: None,
        }
    }

    pub struct Log {
        pub events: Vec<Event>,
    }

    impl Log {
        pub fn start() -> Log {
            Log { events: Vec::new() }
        }

        fn push(
            &mut self,
            ts: String,
            kind: EventKind,
            commit_sha: Option<String>,
            payload: serde_json::Value,
        ) -> String {
            let id = format!("event-{}", self.events.len() + 1);
            self.events.push(Event {
                seq: self.events.len() as i64 + 1,
                id: id.clone(),
                ts,
                kind,
                commit_sha,
                payload,
                prev_hash: String::new(),
                hash: String::new(),
            });
            id
        }

        /// A commit at `secs` from the base timestamp.
        pub fn commit(
            &mut self,
            secs: i64,
            prefix: &str,
            message: &str,
            files: Vec<CommitFile>,
        ) -> String {
            let sha = sha(prefix);
            let payload = CommitPayload {
                sha: sha.clone(),
                message: message.to_string(),
                author: "Test Person <test@example.com>".to_string(),
                parents: vec![],
                files,
                secs_since_prev: None,
            };
            self.push(
                at(secs),
                EventKind::Commit,
                Some(sha.clone()),
                serde_json::to_value(payload).unwrap(),
            );
            sha
        }

        /// An AI session anchored to `sha`.
        pub fn ai(&mut self, secs: i64, sha: &str, files: &[(&str, u64, u64)]) -> String {
            let payload = AiSessionPayload {
                tool: "claude-code".to_string(),
                model: Some("opus".to_string()),
                session_id: format!("s-{}", self.events.len() + 1),
                prompt: Some("write it".to_string()),
                response_excerpt: None,
                files: files
                    .iter()
                    .map(|(path, ai_lines, human)| AiSessionFile {
                        path: path.to_string(),
                        ai_lines: *ai_lines,
                        ai_lines_modified_by_human: *human,
                    })
                    .collect(),
                accepted: true,
            };
            self.push(
                at(secs),
                EventKind::AiSession,
                Some(sha.to_string()),
                serde_json::to_value(payload).unwrap(),
            )
        }

        /// A `madelog note`.
        pub fn decision(&mut self, secs: i64, text: &str, private: bool) -> String {
            self.push(
                at(secs),
                EventKind::Decision,
                None,
                serde_json::json!({ "text": text, "private": private }),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testdata::*;
    use super::*;

    const HOUR: i64 = 3600;
    const DAY: i64 = 86_400;

    fn kinds(patterns: &[PatternPayload]) -> Vec<PatternKind> {
        patterns.iter().map(|p| p.pattern).collect()
    }

    fn only(patterns: &[PatternPayload], kind: PatternKind) -> Vec<PatternPayload> {
        patterns
            .iter()
            .filter(|p| p.pattern == kind)
            .cloned()
            .collect()
    }

    // --- revert -----------------------------------------------------------

    #[test]
    fn a_revert_message_is_a_revert() {
        let mut log = Log::start();
        log.commit(
            0,
            "a1b2c3d",
            "feat: add the parser",
            vec![file("src/parse.rs", 40, 0)],
        );
        log.commit(
            2 * HOUR,
            "b2c3d4e",
            "Revert \"feat: add the parser\"",
            vec![file("src/parse.rs", 0, 40)],
        );

        let found = only(&detect(&log.events), PatternKind::Revert);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].commits, vec![sha("a1b2c3d"), sha("b2c3d4e")]);
        assert_eq!(
            found[0].detail,
            "Reverted 1 file changed in a1b2c3d two hours earlier."
        );
    }

    #[test]
    fn inverse_file_changes_are_a_revert_without_the_word() {
        let mut log = Log::start();
        log.commit(
            0,
            "a1b2c3d",
            "feat: two files",
            vec![file("a.rs", 10, 2), file("b.rs", 3, 0)],
        );
        log.commit(
            HOUR,
            "b2c3d4e",
            "go back to the old shape",
            vec![file("a.rs", 2, 10), file("b.rs", 0, 3)],
        );

        let found = only(&detect(&log.events), PatternKind::Revert);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0].detail,
            "Reverted 2 files changed in a1b2c3d one hour earlier."
        );
    }

    #[test]
    fn a_plain_commit_is_not_a_revert() {
        let mut log = Log::start();
        log.commit(
            0,
            "a1b2c3d",
            "feat: add the parser",
            vec![file("src/parse.rs", 40, 0)],
        );
        log.commit(
            HOUR,
            "b2c3d4e",
            "feat: use the parser",
            vec![file("src/parse.rs", 5, 1)],
        );

        assert!(only(&detect(&log.events), PatternKind::Revert).is_empty());
    }

    #[test]
    fn an_inverse_change_outside_the_window_is_not_a_revert() {
        let mut log = Log::start();
        log.commit(
            0,
            "a1b2c3d",
            "feat: add the parser",
            vec![file("src/parse.rs", 40, 0)],
        );
        log.commit(
            REVERT_WINDOW_SECS + 1,
            "b2c3d4e",
            "drop the parser",
            vec![file("src/parse.rs", 0, 40)],
        );

        assert!(only(&detect(&log.events), PatternKind::Revert).is_empty());
    }

    #[test]
    fn two_even_edits_to_the_same_file_are_not_a_revert() {
        let mut log = Log::start();
        log.commit(
            0,
            "a1b2c3d",
            "tune the threshold",
            vec![file("src/parse.rs", 3, 3)],
        );
        log.commit(
            HOUR,
            "b2c3d4e",
            "tune it again",
            vec![file("src/parse.rs", 3, 3)],
        );

        assert!(only(&detect(&log.events), PatternKind::Revert).is_empty());
    }

    #[test]
    fn a_revert_message_without_a_match_still_counts() {
        let mut log = Log::start();
        log.commit(
            0,
            "a1b2c3d",
            "Revert something from before madelog",
            vec![file("a.rs", 0, 9)],
        );

        let found = only(&detect(&log.events), PatternKind::Revert);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].commits, vec![sha("a1b2c3d")]);
        assert_eq!(found[0].detail, "a1b2c3d undoes earlier work in 1 file.");
    }

    // --- retry ------------------------------------------------------------

    #[test]
    fn three_near_identical_commits_are_a_retry() {
        let mut log = Log::start();
        let files = || vec![file("src/parse.rs", 4, 4), file("src/lex.rs", 1, 1)];
        log.commit(0, "a1b2c3d", "fix the parser offset", files());
        log.commit(20 * 60, "b2c3d4e", "fix the parser offset again", files());
        log.commit(
            50 * 60,
            "c3d4e5f",
            "fix the parser offset properly",
            files(),
        );

        let found = only(&detect(&log.events), PatternKind::Retry);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].commits.len(), 3);
        assert_eq!(
            found[0].detail,
            "Three commits in 50 minutes reworked the same 2 files."
        );
    }

    #[test]
    fn two_commits_are_not_a_retry() {
        let mut log = Log::start();
        let files = || vec![file("src/parse.rs", 4, 4)];
        log.commit(0, "a1b2c3d", "fix the parser offset", files());
        log.commit(20 * 60, "b2c3d4e", "fix the parser offset again", files());

        assert!(only(&detect(&log.events), PatternKind::Retry).is_empty());
    }

    #[test]
    fn a_third_commit_just_outside_two_hours_breaks_the_retry() {
        let mut log = Log::start();
        let files = || vec![file("src/parse.rs", 4, 4)];
        log.commit(0, "a1b2c3d", "fix the parser offset", files());
        log.commit(60 * 60, "b2c3d4e", "fix the parser offset again", files());
        log.commit(
            RETRY_WINDOW_SECS + 1,
            "c3d4e5f",
            "fix the parser offset properly",
            files(),
        );

        assert!(only(&detect(&log.events), PatternKind::Retry).is_empty());
    }

    #[test]
    fn different_files_break_the_retry() {
        let mut log = Log::start();
        log.commit(
            0,
            "a1b2c3d",
            "fix the parser offset",
            vec![file("src/parse.rs", 4, 4)],
        );
        log.commit(
            600,
            "b2c3d4e",
            "fix the parser offset again",
            vec![file("src/parse.rs", 4, 4)],
        );
        log.commit(
            900,
            "c3d4e5f",
            "fix the parser offset properly",
            vec![file("src/lex.rs", 4, 4)],
        );

        assert!(only(&detect(&log.events), PatternKind::Retry).is_empty());
    }

    #[test]
    fn unrelated_messages_break_the_retry() {
        let mut log = Log::start();
        let files = || vec![file("src/parse.rs", 4, 4)];
        log.commit(0, "a1b2c3d", "fix the parser offset", files());
        log.commit(600, "b2c3d4e", "fix the parser offset again", files());
        log.commit(900, "c3d4e5f", "document the release process", files());

        assert!(only(&detect(&log.events), PatternKind::Retry).is_empty());
    }

    #[test]
    fn a_retry_run_is_reported_once_and_does_not_overlap_the_next() {
        let mut log = Log::start();
        let files = || vec![file("src/parse.rs", 4, 4)];
        for (i, name) in ["a1b2c3d", "b2c3d4e", "c3d4e5f", "d4e5f60"]
            .iter()
            .enumerate()
        {
            log.commit(i as i64 * 600, name, "fix the parser offset", files());
        }

        let found = only(&detect(&log.events), PatternKind::Retry);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].commits.len(), 4);
    }

    // --- fix_after_ai -----------------------------------------------------

    #[test]
    fn a_fix_after_an_ai_commit_is_a_pattern() {
        let mut log = Log::start();
        let first = log.commit(
            0,
            "a1b2c3d",
            "feat: generated parser",
            vec![file("src/parse.rs", 80, 0)],
        );
        log.ai(0, &first, &[("src/parse.rs", 80, 0)]);
        log.commit(
            2 * HOUR,
            "b2c3d4e",
            "fix: the parser was wrong",
            vec![file("src/parse.rs", 6, 6)],
        );

        let found = only(&detect(&log.events), PatternKind::FixAfterAi);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].commits, vec![sha("a1b2c3d"), sha("b2c3d4e")]);
        assert_eq!(
            found[0].detail,
            "b2c3d4e reworked AI-written code in src/parse.rs two hours after a1b2c3d."
        );
    }

    #[test]
    fn human_reworked_ai_lines_count_even_without_a_fix_word() {
        let mut log = Log::start();
        let first = log.commit(
            0,
            "a1b2c3d",
            "feat: generated parser",
            vec![file("src/parse.rs", 80, 0)],
        );
        log.ai(0, &first, &[("src/parse.rs", 80, 0)]);
        let second = log.commit(
            HOUR,
            "b2c3d4e",
            "tidy the parser",
            vec![file("src/parse.rs", 6, 6)],
        );
        log.ai(HOUR, &second, &[("src/parse.rs", 10, 7)]);

        assert_eq!(only(&detect(&log.events), PatternKind::FixAfterAi).len(), 1);
    }

    #[test]
    fn a_fix_a_day_and_a_second_later_is_too_late() {
        let mut log = Log::start();
        let first = log.commit(
            0,
            "a1b2c3d",
            "feat: generated parser",
            vec![file("src/parse.rs", 80, 0)],
        );
        log.ai(0, &first, &[("src/parse.rs", 80, 0)]);
        log.commit(
            FIX_AFTER_AI_WINDOW_SECS + 1,
            "b2c3d4e",
            "fix: the parser was wrong",
            vec![file("src/parse.rs", 6, 6)],
        );

        assert!(only(&detect(&log.events), PatternKind::FixAfterAi).is_empty());
    }

    #[test]
    fn a_fix_to_other_files_is_not_a_fix_after_ai() {
        let mut log = Log::start();
        let first = log.commit(
            0,
            "a1b2c3d",
            "feat: generated parser",
            vec![file("src/parse.rs", 80, 0)],
        );
        log.ai(0, &first, &[("src/parse.rs", 80, 0)]);
        log.commit(
            HOUR,
            "b2c3d4e",
            "fix: the readme was wrong",
            vec![file("README.md", 2, 2)],
        );

        assert!(only(&detect(&log.events), PatternKind::FixAfterAi).is_empty());
    }

    #[test]
    fn a_fix_after_a_human_commit_is_not_a_fix_after_ai() {
        let mut log = Log::start();
        log.commit(
            0,
            "a1b2c3d",
            "feat: handwritten parser",
            vec![file("src/parse.rs", 80, 0)],
        );
        log.commit(
            HOUR,
            "b2c3d4e",
            "fix: the parser was wrong",
            vec![file("src/parse.rs", 6, 6)],
        );

        assert!(only(&detect(&log.events), PatternKind::FixAfterAi).is_empty());
    }

    #[test]
    fn fix_words_are_matched_on_word_boundaries() {
        assert!(mentions_fix("fix: the parser"));
        assert!(mentions_fix("this broke everything"));
        assert!(mentions_fix("oops"));
        assert!(mentions_fix("fixed the bugs"));
        assert!(!mentions_fix("prefix the paths"));
        assert!(!mentions_fix("feat: add the parser"));
    }

    // --- stall ------------------------------------------------------------

    #[test]
    fn a_long_gap_then_a_wide_commit_is_a_stall() {
        let mut log = Log::start();
        log.commit(
            0,
            "a1b2c3d",
            "feat: the beginning",
            vec![file("a.rs", 3, 0)],
        );
        let wide: Vec<CommitFile> = (0..10)
            .map(|i| file(&format!("src/f{i}.rs"), 5, 0))
            .collect();
        log.commit(6 * DAY, "b2c3d4e", "feat: the new shape", wide);

        let found = only(&detect(&log.events), PatternKind::Stall);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].commits, vec![sha("a1b2c3d"), sha("b2c3d4e")]);
        assert_eq!(
            found[0].detail,
            "After six days without commits, b2c3d4e changed 10 files (50 lines)."
        );
    }

    #[test]
    fn a_long_gap_then_a_big_commit_by_lines_is_a_stall() {
        let mut log = Log::start();
        log.commit(
            0,
            "a1b2c3d",
            "feat: the beginning",
            vec![file("a.rs", 3, 0)],
        );
        log.commit(
            5 * DAY,
            "b2c3d4e",
            "feat: the new shape",
            vec![file("a.rs", 300, 0)],
        );

        assert_eq!(only(&detect(&log.events), PatternKind::Stall).len(), 1);
    }

    #[test]
    fn a_gap_just_under_five_days_is_not_a_stall() {
        let mut log = Log::start();
        log.commit(
            0,
            "a1b2c3d",
            "feat: the beginning",
            vec![file("a.rs", 3, 0)],
        );
        log.commit(
            STALL_GAP_SECS - 1,
            "b2c3d4e",
            "feat: the new shape",
            vec![file("a.rs", 400, 0)],
        );

        assert!(only(&detect(&log.events), PatternKind::Stall).is_empty());
    }

    #[test]
    fn a_long_gap_then_a_small_commit_is_not_a_stall() {
        let mut log = Log::start();
        log.commit(
            0,
            "a1b2c3d",
            "feat: the beginning",
            vec![file("a.rs", 3, 0)],
        );
        let nine: Vec<CommitFile> = (0..9)
            .map(|i| file(&format!("src/f{i}.rs"), 5, 0))
            .collect();
        log.commit(9 * DAY, "b2c3d4e", "chore: tidy", nine);

        assert!(only(&detect(&log.events), PatternKind::Stall).is_empty());
    }

    // --- the whole thing --------------------------------------------------

    #[test]
    fn detection_is_deterministic_and_chronological() {
        let mut log = Log::start();
        let first = log.commit(
            0,
            "a1b2c3d",
            "feat: generated parser",
            vec![file("src/parse.rs", 80, 0)],
        );
        log.ai(0, &first, &[("src/parse.rs", 80, 0)]);
        log.commit(
            2 * HOUR,
            "b2c3d4e",
            "fix: the parser was wrong",
            vec![file("src/parse.rs", 6, 6)],
        );
        let wide: Vec<CommitFile> = (0..12)
            .map(|i| file(&format!("src/f{i}.rs"), 5, 0))
            .collect();
        log.commit(8 * DAY, "c3d4e5f", "feat: the new shape", wide);

        let found = detect(&log.events);
        assert_eq!(
            kinds(&found),
            vec![PatternKind::FixAfterAi, PatternKind::Stall]
        );
        assert_eq!(found, detect(&log.events), "same input, same output");
    }

    #[test]
    fn an_empty_log_has_no_patterns() {
        assert!(detect(&[]).is_empty());
    }

    #[test]
    fn ai_sessions_are_attached_to_their_commit() {
        let mut log = Log::start();
        let sha = log.commit(
            0,
            "a1b2c3d",
            "feat: parser",
            vec![file("src/parse.rs", 80, 0)],
        );
        log.ai(0, &sha, &[("src/parse.rs", 80, 12)]);

        let commits = commits(&log.events);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].ai.len(), 1);
        assert_eq!(commits[0].ai_event_ids.len(), 1);
        assert_eq!(commits[0].ai_lines(), 80);
        assert_eq!(commits[0].ai_lines_reworked(), 12);
        assert_eq!(commits[0].short(), "a1b2c3d");
    }

    // --- text helpers -----------------------------------------------------

    #[test]
    fn gaps_read_like_english() {
        assert_eq!(humanize_gap(0), "less than a minute");
        assert_eq!(humanize_gap(30), "less than a minute");
        assert_eq!(humanize_gap(60), "one minute");
        assert_eq!(humanize_gap(50 * 60), "50 minutes");
        assert_eq!(humanize_gap(2 * HOUR), "two hours");
        assert_eq!(humanize_gap(6 * DAY), "six days");
        assert_eq!(humanize_gap(40 * DAY), "40 days");
    }

    #[test]
    fn quoted_messages_are_one_line_and_short() {
        assert_eq!(quotable("feat: a thing.\nmore body", 40), "feat: a thing");
        assert_eq!(
            quotable("a very long commit message indeed", 10),
            "a very lon..."
        );
    }
}
