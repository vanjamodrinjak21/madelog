//! Weekly buckets: Monday to Sunday, in local time, counted from the first
//! commit.
//!
//! A bucket carries the *structured* facts of a week — commit messages with
//! file counts, the decisions you wrote down, the patterns analysis found.
//! Never a diff excerpt, never a prompt: whatever a model sees, it sees from
//! here, and this is the only place that decides what that is.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate};

use crate::event::{CommitPayload, DecisionPayload, Event, EventKind, PatternPayload};

/// At most this many commit lines go into a week's text; the rest are counted.
const MAX_COMMIT_LINES: usize = 40;

/// How many files a week names as the ones most of the work landed in.
pub const TOP_FILES: usize = 3;

/// One commit, reduced to what a summary may know about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitLine {
    pub date: NaiveDate,
    /// First line of the commit message.
    pub message: String,
    pub files: usize,
    pub added: u64,
    pub removed: u64,
    pub paths: Vec<String>,
}

/// One detected pattern, reduced the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternLine {
    pub kind: String,
    pub detail: String,
}

/// A Monday-to-Sunday week of recorded work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Week {
    /// 1 for the week containing the first commit, counting calendar weeks.
    pub index: u32,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub commits: Vec<CommitLine>,
    /// Decision texts. Private decisions never get here.
    pub decisions: Vec<String>,
    pub patterns: Vec<PatternLine>,
}

impl Week {
    fn new(origin: NaiveDate, index: u32) -> Week {
        let start = origin + Duration::days(7 * i64::from(index - 1));
        Week {
            index,
            start,
            end: start + Duration::days(6),
            commits: Vec::new(),
            decisions: Vec::new(),
            patterns: Vec::new(),
        }
    }

    /// `YYYY-MM-DD` of the Monday, as the summary payload stores it.
    pub fn week_start(&self) -> String {
        self.start.format("%Y-%m-%d").to_string()
    }

    /// `YYYY-MM-DD` of the Sunday.
    pub fn week_end(&self) -> String {
        self.end.format("%Y-%m-%d").to_string()
    }

    /// Distinct local days that carry at least one commit.
    pub fn days_active(&self) -> usize {
        let mut days: Vec<NaiveDate> = self.commits.iter().map(|c| c.date).collect();
        days.sort_unstable();
        days.dedup();
        days.len()
    }

    /// The [`TOP_FILES`] paths touched by the most commits, busiest first.
    pub fn top_files(&self) -> Vec<String> {
        let mut counts: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
        for commit in &self.commits {
            let lines = commit.added + commit.removed;
            for path in &commit.paths {
                let entry = counts.entry(path.as_str()).or_insert((0, 0));
                entry.0 += 1;
                entry.1 += lines;
            }
        }
        let mut ranked: Vec<(&str, (usize, u64))> = counts.into_iter().collect();
        // Most commits first, then most lines, then alphabetically: no ties,
        // so the same week always produces the same sentence.
        ranked.sort_by(|a, b| {
            b.1 .0
                .cmp(&a.1 .0)
                .then(b.1 .1.cmp(&a.1 .1))
                .then(a.0.cmp(b.0))
        });
        ranked
            .into_iter()
            .take(TOP_FILES)
            .map(|(path, _)| path.to_string())
            .collect()
    }

    fn is_empty(&self) -> bool {
        self.commits.is_empty() && self.decisions.is_empty() && self.patterns.is_empty()
    }

    /// The text a summariser is given. Facts only, no diffs, no prompts.
    pub fn input_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "Week {}: {} to {}",
            self.index,
            self.week_start(),
            self.week_end()
        );

        if self.commits.is_empty() {
            out.push_str("Commits: none.\n");
        } else {
            let days = self.days_active();
            let _ = writeln!(
                out,
                "Commits ({}) on {}:",
                self.commits.len(),
                count(days, "day", "days")
            );
            for commit in self.commits.iter().take(MAX_COMMIT_LINES) {
                let _ = writeln!(
                    out,
                    "- {}: {} ({}, +{}/-{})",
                    commit.date,
                    commit.message,
                    count(commit.files, "file", "files"),
                    commit.added,
                    commit.removed
                );
            }
            if self.commits.len() > MAX_COMMIT_LINES {
                let rest = self.commits.len() - MAX_COMMIT_LINES;
                let plural = if rest == 1 { "" } else { "s" };
                let _ = writeln!(out, "- and {rest} more commit{plural}");
            }
            let top = self.top_files();
            if !top.is_empty() {
                let _ = writeln!(out, "Files touched most: {}", top.join(", "));
            }
        }

        if !self.decisions.is_empty() {
            let _ = writeln!(out, "Decisions ({}):", self.decisions.len());
            for decision in &self.decisions {
                let _ = writeln!(out, "- {decision}");
            }
        }

        if !self.patterns.is_empty() {
            let _ = writeln!(out, "Patterns ({}):", self.patterns.len());
            for pattern in &self.patterns {
                let _ = writeln!(out, "- {}: {}", pattern.kind, pattern.detail);
            }
        }

        out
    }
}

/// `1 file` / `2 files`, so no sentence ever reads "1 files".
pub(crate) fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// Bucket the log into weeks, in the machine's own timezone.
pub fn bucket(events: &[Event]) -> Vec<Week> {
    bucket_with(events, local_date)
}

/// The local calendar date of an RFC 3339 timestamp.
fn local_date(ts: &str) -> Option<NaiveDate> {
    Some(
        DateTime::parse_from_rfc3339(ts)
            .ok()?
            .with_timezone(&Local)
            .date_naive(),
    )
}

/// The bucketing itself, with the clock handed in so tests can pretend to live
/// anywhere — including in a week that a daylight-saving change cuts in half.
fn bucket_with(events: &[Event], to_date: impl Fn(&str) -> Option<NaiveDate>) -> Vec<Week> {
    let Some(origin) = events
        .iter()
        .filter(|e| e.kind == EventKind::Commit)
        .find_map(|e| to_date(&e.ts))
        .map(monday_of)
    else {
        // No commits, no weeks: pop counts weeks from the first commit.
        return Vec::new();
    };

    let mut weeks: BTreeMap<u32, Week> = BTreeMap::new();
    for event in events {
        let Some(date) = to_date(&event.ts) else {
            continue;
        };
        let index = week_index(origin, date);
        match event.kind {
            EventKind::Commit => {
                let Ok(payload) = event.payload_as::<CommitPayload>() else {
                    continue;
                };
                let line = CommitLine {
                    date,
                    message: first_line(&payload.message),
                    files: payload.files.len(),
                    added: payload.files.iter().map(|f| f.added).sum(),
                    removed: payload.files.iter().map(|f| f.removed).sum(),
                    paths: payload.files.iter().map(|f| f.path.clone()).collect(),
                };
                weeks
                    .entry(index)
                    .or_insert_with(|| Week::new(origin, index))
                    .commits
                    .push(line);
            }
            EventKind::Decision => {
                let Ok(payload) = event.payload_as::<DecisionPayload>() else {
                    continue;
                };
                // A private note is chained, counted and never read out loud.
                if payload.private {
                    continue;
                }
                let text = payload.text.trim().to_string();
                if text.is_empty() {
                    continue;
                }
                weeks
                    .entry(index)
                    .or_insert_with(|| Week::new(origin, index))
                    .decisions
                    .push(text);
            }
            EventKind::Pattern => {
                let Ok(payload) = event.payload_as::<PatternPayload>() else {
                    continue;
                };
                let line = PatternLine {
                    kind: serde_json::to_value(payload.pattern)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_else(|| "pattern".to_string()),
                    detail: first_line(&payload.detail),
                };
                weeks
                    .entry(index)
                    .or_insert_with(|| Week::new(origin, index))
                    .patterns
                    .push(line);
            }
            _ => continue,
        }
    }

    weeks.into_values().filter(|w| !w.is_empty()).collect()
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

/// The Monday of the week `date` falls in.
fn monday_of(date: NaiveDate) -> NaiveDate {
    date - Duration::days(i64::from(date.weekday().num_days_from_monday()))
}

/// Which week `date` belongs to, counting from `origin`. Calendar arithmetic,
/// not seconds: a week that loses or gains an hour is still seven days long.
/// Anything before the first commit is clamped into week 1.
fn week_index(origin: NaiveDate, date: NaiveDate) -> u32 {
    let days = (date - origin).num_days();
    if days < 0 {
        1
    } else {
        (days / 7) as u32 + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{CommitFile, FileStatus, PatternKind, Payload};

    fn event(ts: &str, kind: EventKind, payload: serde_json::Value) -> Event {
        Event {
            seq: 1,
            id: "id".into(),
            ts: ts.into(),
            kind,
            commit_sha: None,
            payload,
            prev_hash: crate::event::GENESIS_PREV_HASH.into(),
            hash: "h".into(),
        }
    }

    fn commit(ts: &str, message: &str, files: &[(&str, u64, u64)]) -> Event {
        let payload = Payload::Commit(CommitPayload {
            sha: "a".repeat(40),
            message: message.into(),
            author: "Test Person".into(),
            parents: vec![],
            files: files
                .iter()
                .map(|(path, added, removed)| CommitFile {
                    path: (*path).into(),
                    added: *added,
                    removed: *removed,
                    status: FileStatus::Modified,
                    diff_excerpt: Some("- secret line\n+ another secret line".into()),
                })
                .collect(),
            secs_since_prev: None,
        });
        event(ts, EventKind::Commit, payload.to_value())
    }

    fn decision(ts: &str, text: &str, private: bool) -> Event {
        let payload = Payload::Decision(DecisionPayload {
            text: text.into(),
            private,
        });
        event(ts, EventKind::Decision, payload.to_value())
    }

    fn pattern(ts: &str, kind: PatternKind, detail: &str) -> Event {
        let payload = Payload::Pattern(PatternPayload {
            pattern: kind,
            commits: vec!["b".repeat(40)],
            detail: detail.into(),
        });
        event(ts, EventKind::Pattern, payload.to_value())
    }

    /// Everything is already UTC in these tests, so "local" is the identity.
    fn utc_date(ts: &str) -> Option<NaiveDate> {
        Some(DateTime::parse_from_rfc3339(ts).ok()?.date_naive())
    }

    fn date(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn a_log_without_commits_has_no_weeks() {
        let events = vec![decision("2026-09-17T10:00:00Z", "no commits yet", false)];
        assert!(bucket_with(&events, utc_date).is_empty());
    }

    #[test]
    fn weeks_start_on_monday_and_end_on_sunday() {
        // 2026-09-17 is a Thursday.
        let events = vec![commit("2026-09-17T10:00:00Z", "first", &[("a.rs", 1, 0)])];
        let weeks = bucket_with(&events, utc_date);
        assert_eq!(weeks.len(), 1);
        assert_eq!(weeks[0].index, 1);
        assert_eq!(weeks[0].week_start(), "2026-09-14");
        assert_eq!(weeks[0].week_end(), "2026-09-20");
    }

    #[test]
    fn a_week_that_crosses_a_month_boundary_stays_one_week() {
        // Monday 2026-08-31 through Sunday 2026-09-06 is one week; Monday
        // 2026-09-07 starts the next one.
        let events = vec![
            commit("2026-08-31T09:00:00Z", "monday", &[("a.rs", 1, 0)]),
            commit("2026-09-01T09:00:00Z", "tuesday", &[("a.rs", 1, 0)]),
            commit("2026-09-06T22:00:00Z", "sunday", &[("a.rs", 1, 0)]),
            commit("2026-09-07T08:00:00Z", "next monday", &[("a.rs", 1, 0)]),
        ];
        let weeks = bucket_with(&events, utc_date);
        assert_eq!(weeks.len(), 2);
        assert_eq!(weeks[0].commits.len(), 3);
        assert_eq!(weeks[0].week_start(), "2026-08-31");
        assert_eq!(weeks[0].week_end(), "2026-09-06");
        assert_eq!(weeks[1].index, 2);
        assert_eq!(weeks[1].commits.len(), 1);
        assert_eq!(weeks[1].week_start(), "2026-09-07");
    }

    #[test]
    fn a_year_boundary_does_not_restart_the_count() {
        let events = vec![
            commit("2026-12-28T09:00:00Z", "monday", &[("a.rs", 1, 0)]),
            commit("2027-01-04T09:00:00Z", "next monday", &[("a.rs", 1, 0)]),
        ];
        let weeks = bucket_with(&events, utc_date);
        assert_eq!(weeks.len(), 2);
        assert_eq!(weeks[1].index, 2);
        assert_eq!(weeks[1].week_start(), "2027-01-04");
    }

    #[test]
    fn empty_weeks_are_skipped_but_the_numbering_keeps_counting() {
        let events = vec![
            commit("2026-09-14T09:00:00Z", "week one", &[("a.rs", 1, 0)]),
            commit("2026-09-30T09:00:00Z", "week three", &[("a.rs", 1, 0)]),
        ];
        let weeks = bucket_with(&events, utc_date);
        assert_eq!(weeks.len(), 2);
        assert_eq!(weeks[1].index, 3);
        assert_eq!(weeks[1].week_start(), "2026-09-28");
    }

    #[test]
    fn a_daylight_saving_change_does_not_move_the_week_boundary() {
        // Europe/Zagreb: UTC+1 until 01:00 UTC on Sunday 2026-03-29, UTC+2
        // after. The week Mon 2026-03-23 – Sun 2026-03-29 is 167 hours long,
        // and must still be exactly one bucket of seven calendar days.
        fn zagreb(ts: &str) -> Option<NaiveDate> {
            let utc = DateTime::parse_from_rfc3339(ts).ok()?.naive_utc();
            let switch: chrono::NaiveDateTime = "2026-03-29T01:00:00".parse().ok()?;
            let offset = if utc < switch { 1 } else { 2 };
            Some((utc + Duration::hours(offset)).date())
        }

        let events = vec![
            commit("2026-03-23T08:00:00Z", "monday", &[("a.rs", 1, 0)]),
            // 23:30 UTC on Saturday is already Sunday 00:30 local: still week 1.
            commit("2026-03-28T23:30:00Z", "late saturday", &[("a.rs", 1, 0)]),
            // 01:30 UTC on Sunday is 03:30 local after the clocks jump.
            commit("2026-03-29T01:30:00Z", "after the jump", &[("a.rs", 1, 0)]),
            // 22:30 UTC on Sunday is already Monday 00:30 local: week 2.
            commit("2026-03-29T22:30:00Z", "monday again", &[("a.rs", 1, 0)]),
        ];
        let weeks = bucket_with(&events, zagreb);
        assert_eq!(weeks.len(), 2);
        assert_eq!(weeks[0].week_start(), "2026-03-23");
        assert_eq!(weeks[0].week_end(), "2026-03-29");
        assert_eq!(weeks[0].commits.len(), 3);
        assert_eq!(weeks[1].week_start(), "2026-03-30");
        assert_eq!(weeks[1].commits.len(), 1);
        assert_eq!((weeks[1].start - weeks[0].start).num_days(), 7);
    }

    #[test]
    fn a_timezone_can_push_an_event_into_the_previous_week() {
        // Pacific/Honolulu, UTC-10 and no daylight saving: Monday 09:00 UTC is
        // still Sunday evening there.
        fn honolulu(ts: &str) -> Option<NaiveDate> {
            let utc = DateTime::parse_from_rfc3339(ts).ok()?.naive_utc();
            Some((utc - Duration::hours(10)).date())
        }
        let events = vec![
            commit("2026-09-08T12:00:00Z", "tuesday", &[("a.rs", 1, 0)]),
            // Monday morning in UTC is still Sunday evening in Honolulu.
            commit(
                "2026-09-14T05:00:00Z",
                "still sunday there",
                &[("a.rs", 1, 0)],
            ),
        ];
        let weeks = bucket_with(&events, honolulu);
        assert_eq!(weeks.len(), 1);
        assert_eq!(weeks[0].week_start(), "2026-09-07");
        assert_eq!(weeks[0].commits.len(), 2);

        // The same two events split in two in UTC. The timezone is the whole
        // difference, which is why it is the machine's own and not a guess.
        assert_eq!(bucket_with(&events, utc_date).len(), 2);
    }

    #[test]
    fn private_decisions_never_enter_a_bucket() {
        let events = vec![
            commit("2026-09-14T09:00:00Z", "first", &[("a.rs", 1, 0)]),
            decision("2026-09-15T09:00:00Z", "chose sqlite", false),
            decision("2026-09-15T10:00:00Z", "burned out this week", true),
        ];
        let weeks = bucket_with(&events, utc_date);
        assert_eq!(weeks[0].decisions, vec!["chose sqlite".to_string()]);
    }

    #[test]
    fn notes_written_before_the_first_commit_land_in_week_one() {
        let events = vec![
            decision("2026-09-10T09:00:00Z", "decided to start", false),
            commit("2026-09-14T09:00:00Z", "first", &[("a.rs", 1, 0)]),
        ];
        let weeks = bucket_with(&events, utc_date);
        assert_eq!(weeks.len(), 1);
        assert_eq!(weeks[0].index, 1);
        assert_eq!(weeks[0].decisions.len(), 1);
    }

    #[test]
    fn top_files_ranks_by_commits_then_lines_then_name() {
        let events = vec![
            commit(
                "2026-09-14T09:00:00Z",
                "one",
                &[("src/store.rs", 40, 2), ("README.md", 1, 0)],
            ),
            commit(
                "2026-09-15T09:00:00Z",
                "two",
                &[("src/store.rs", 10, 1), ("src/event.rs", 30, 0)],
            ),
            commit("2026-09-16T09:00:00Z", "three", &[("src/event.rs", 5, 5)]),
        ];
        let weeks = bucket_with(&events, utc_date);
        assert_eq!(
            weeks[0].top_files(),
            vec![
                "src/store.rs".to_string(),
                "src/event.rs".to_string(),
                "README.md".to_string()
            ]
        );
        assert_eq!(weeks[0].days_active(), 3);
    }

    #[test]
    fn the_input_text_carries_facts_and_never_a_diff() {
        let events = vec![
            commit(
                "2026-09-14T09:00:00Z",
                "add the event store\n\nlonger body that is not a summary's business",
                &[("src/store.rs", 120, 3)],
            ),
            decision(
                "2026-09-15T09:00:00Z",
                "chose sqlite over a json file",
                false,
            ),
            decision("2026-09-15T10:00:00Z", "never said out loud", true),
            pattern(
                "2026-09-16T09:00:00Z",
                PatternKind::FixAfterAi,
                "two commits fixed files an AI session wrote",
            ),
        ];
        let weeks = bucket_with(&events, utc_date);
        let text = weeks[0].input_text();

        assert_eq!(
            text,
            "Week 1: 2026-09-14 to 2026-09-20\n\
             Commits (1) on 1 day:\n\
             - 2026-09-14: add the event store (1 file, +120/-3)\n\
             Files touched most: src/store.rs\n\
             Decisions (1):\n\
             - chose sqlite over a json file\n\
             Patterns (1):\n\
             - fix_after_ai: two commits fixed files an AI session wrote\n"
        );
        assert!(!text.contains("secret line"), "no diff excerpt: {text}");
        assert!(!text.contains("never said out loud"), "{text}");
        assert!(!text.contains("longer body"), "{text}");
    }

    #[test]
    fn a_long_week_stops_listing_and_starts_counting() {
        let events: Vec<Event> = (0..MAX_COMMIT_LINES + 5)
            .map(|i| {
                commit(
                    &format!("2026-09-14T09:{:02}:00Z", i % 60),
                    &format!("commit {i}"),
                    &[("a.rs", 1, 0)],
                )
            })
            .collect();
        let weeks = bucket_with(&events, utc_date);
        let text = weeks[0].input_text();
        assert!(text.contains("- and 5 more commits"), "{text}");
        assert!(!text.contains("commit 41"), "{text}");
    }

    #[test]
    fn monday_of_is_idempotent_on_a_monday() {
        assert_eq!(monday_of(date("2026-09-14")), date("2026-09-14"));
        assert_eq!(monday_of(date("2026-09-20")), date("2026-09-14"));
    }
}
