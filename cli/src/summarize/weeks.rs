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

/// One file inside a commit, and the lines that commit changed in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLines {
    pub path: String,
    /// `added + removed` for this file alone, never the whole commit's total.
    pub lines: u64,
}

/// One commit, reduced to what a summary may know about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitLine {
    pub date: NaiveDate,
    /// First line of the commit message.
    pub message: String,
    pub files: usize,
    pub added: u64,
    pub removed: u64,
    pub paths: Vec<FileLines>,
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
            for file in &commit.paths {
                // Each path earns the lines it changed itself, not the whole
                // commit's total, or a one-line typo rides on its neighbours.
                let entry = counts.entry(file.path.as_str()).or_insert((0, 0));
                entry.0 += 1;
                entry.1 += file.lines;
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
    // The patterns are detected afresh rather than read back from the
    // `pattern` events in the chain. A stored finding is a record of what
    // the analysis said at the time; a summary must say what the log shows
    // now, and a rule that has since been corrected must not keep speaking
    // through old events.
    let patterns = crate::patterns::detect(events);
    bucket_with(events, &patterns, local_date)
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
fn bucket_with(
    events: &[Event],
    patterns: &[PatternPayload],
    to_date: impl Fn(&str) -> Option<NaiveDate>,
) -> Vec<Week> {
    // When each commit was *made*, by sha. A commit recorded by `madelog
    // backfill` carries `committed_at`, and that is the week it belongs in:
    // bucketing it by `ts` would drop a year of history into the week the
    // backfill ran.
    let commit_ts: BTreeMap<&str, &str> = events
        .iter()
        .filter(|e| e.kind == EventKind::Commit)
        .filter_map(|e| e.commit_sha.as_deref().map(|sha| (sha, work_ts(e))))
        .collect();

    let Some(origin) = events
        .iter()
        .filter(|e| e.kind == EventKind::Commit)
        .filter_map(|e| to_date(work_ts(e)))
        .min()
        .map(monday_of)
    else {
        // No commits, no weeks: madelog counts weeks from the first commit.
        return Vec::new();
    };

    let mut weeks: BTreeMap<u32, Week> = BTreeMap::new();
    for event in events {
        // Stored `pattern` events are not read here: see `bucket`.
        if event.kind == EventKind::Pattern {
            continue;
        }
        let ts = match event.kind {
            EventKind::Commit => work_ts(event),
            _ => event.ts.as_str(),
        };
        let Some(date) = to_date(ts) else {
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
                    paths: payload
                        .files
                        .iter()
                        .map(|f| FileLines {
                            path: f.path.clone(),
                            lines: f.added + f.removed,
                        })
                        .collect(),
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
            _ => continue,
        }
    }

    // A pattern belongs to the week of the last commit it is about, which is
    // when the thing it describes finished happening. One about a commit this
    // log does not have is not about this log.
    for pattern in patterns {
        let Some(ts) = pattern
            .commits
            .last()
            .and_then(|sha| commit_ts.get(sha.as_str()).copied())
        else {
            continue;
        };
        let Some(date) = to_date(ts) else {
            continue;
        };
        let index = week_index(origin, date);
        weeks
            .entry(index)
            .or_insert_with(|| Week::new(origin, index))
            .patterns
            .push(PatternLine {
                kind: crate::patterns::kind_str(pattern.pattern).to_string(),
                detail: first_line(&pattern.detail),
            });
    }

    weeks.into_values().filter(|w| !w.is_empty()).collect()
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

/// When a commit event says the commit was made: `committed_at` when the
/// payload has one, the moment madelog recorded it otherwise.
fn work_ts(event: &Event) -> &str {
    event
        .payload
        .get("committed_at")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .unwrap_or(event.ts.as_str())
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
        event_on(ts, None, kind, payload)
    }

    fn event_on(
        ts: &str,
        commit_sha: Option<&str>,
        kind: EventKind,
        payload: serde_json::Value,
    ) -> Event {
        Event {
            seq: 1,
            id: "id".into(),
            ts: ts.into(),
            kind,
            commit_sha: commit_sha.map(str::to_string),
            payload,
            prev_hash: crate::event::GENESIS_PREV_HASH.into(),
            hash: "h".into(),
        }
    }

    fn commit(ts: &str, message: &str, files: &[(&str, u64, u64)]) -> Event {
        commit_at(ts, &"a".repeat(40), message, files)
    }

    /// A commit event anchored to `sha`, the way the post-commit hook writes it.
    fn commit_at(ts: &str, sha: &str, message: &str, files: &[(&str, u64, u64)]) -> Event {
        let payload = Payload::Commit(CommitPayload {
            sha: sha.to_string(),
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
            committed_at: None,
        });
        event_on(ts, Some(sha), EventKind::Commit, payload.to_value())
    }

    /// A commit recorded by `madelog backfill`: `ts` is when the backfill ran,
    /// `committed_at` is when the commit was actually made.
    fn backfilled(recorded: &str, made: &str, sha: &str, message: &str) -> Event {
        let mut event = commit_at(recorded, sha, message, &[("src/a.rs", 1, 0)]);
        event.payload["committed_at"] = serde_json::json!(made);
        event
    }

    fn decision(ts: &str, text: &str, private: bool) -> Event {
        let payload = Payload::Decision(DecisionPayload {
            text: text.into(),
            private,
            amends: None,
        });
        event(ts, EventKind::Decision, payload.to_value())
    }

    /// A pattern event anchored to the last commit it covers, the way
    /// `madelog analyze` writes it.
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
        assert!(bucket_with(&events, &[], utc_date).is_empty());
    }

    #[test]
    fn weeks_start_on_monday_and_end_on_sunday() {
        // 2026-09-17 is a Thursday.
        let events = vec![commit("2026-09-17T10:00:00Z", "first", &[("a.rs", 1, 0)])];
        let weeks = bucket_with(&events, &[], utc_date);
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
        let weeks = bucket_with(&events, &[], utc_date);
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
        let weeks = bucket_with(&events, &[], utc_date);
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
        let weeks = bucket_with(&events, &[], utc_date);
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
        let weeks = bucket_with(&events, &[], zagreb);
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
        let weeks = bucket_with(&events, &[], honolulu);
        assert_eq!(weeks.len(), 1);
        assert_eq!(weeks[0].week_start(), "2026-09-07");
        assert_eq!(weeks[0].commits.len(), 2);

        // The same two events split in two in UTC. The timezone is the whole
        // difference, which is why it is the machine's own and not a guess.
        assert_eq!(bucket_with(&events, &[], utc_date).len(), 2);
    }

    #[test]
    fn private_decisions_never_enter_a_bucket() {
        let events = vec![
            commit("2026-09-14T09:00:00Z", "first", &[("a.rs", 1, 0)]),
            decision("2026-09-15T09:00:00Z", "chose sqlite", false),
            decision("2026-09-15T10:00:00Z", "burned out this week", true),
        ];
        let weeks = bucket_with(&events, &[], utc_date);
        assert_eq!(weeks[0].decisions, vec!["chose sqlite".to_string()]);
    }

    #[test]
    fn notes_written_before_the_first_commit_land_in_week_one() {
        let events = vec![
            decision("2026-09-10T09:00:00Z", "decided to start", false),
            commit("2026-09-14T09:00:00Z", "first", &[("a.rs", 1, 0)]),
        ];
        let weeks = bucket_with(&events, &[], utc_date);
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
        let weeks = bucket_with(&events, &[], utc_date);
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
    fn a_pattern_belongs_to_the_week_of_the_commit_it_is_about() {
        // The work is in week 1; the analysis only runs three weeks later.
        let first = "b".repeat(40);
        let last = "c".repeat(40);
        let events = vec![
            commit_at("2026-09-14T09:00:00Z", &first, "first", &[("a.rs", 1, 0)]),
            commit_at(
                "2026-10-05T09:00:00Z",
                &last,
                "much later",
                &[("a.rs", 1, 0)],
            ),
        ];
        let patterns = vec![PatternPayload {
            pattern: PatternKind::Revert,
            commits: vec![first.clone()],
            detail: "reverted a.rs".into(),
        }];
        let weeks = bucket_with(&events, &patterns, utc_date);
        assert_eq!(weeks.len(), 2);
        assert_eq!(weeks[0].week_start(), "2026-09-14");
        assert_eq!(weeks[0].patterns.len(), 1, "the week of the commit");
        assert_eq!(weeks[0].patterns[0].kind, "revert");
        assert!(
            weeks[1].patterns.is_empty(),
            "not the week the analysis happened to run: {:?}",
            weeks[1].patterns
        );
    }
    #[test]
    fn a_pattern_about_a_commit_the_log_does_not_have_is_dropped() {
        let events = vec![commit("2026-09-14T09:00:00Z", "first", &[("a.rs", 1, 0)])];
        let patterns = vec![PatternPayload {
            pattern: PatternKind::Stall,
            commits: vec!["f".repeat(40)],
            detail: "a long gap".into(),
        }];
        let weeks = bucket_with(&events, &patterns, utc_date);
        assert_eq!(weeks.len(), 1);
        assert!(weeks[0].patterns.is_empty(), "{:?}", weeks[0].patterns);

        // Stored pattern events are not read either: the detection is.
        let events = vec![
            commit("2026-09-14T09:00:00Z", "first", &[("a.rs", 1, 0)]),
            event_on(
                "2026-09-21T09:00:00Z",
                None,
                EventKind::Pattern,
                Payload::Pattern(PatternPayload {
                    pattern: PatternKind::Stall,
                    commits: vec![],
                    detail: "a stale finding".into(),
                })
                .to_value(),
            ),
        ];
        let weeks = bucket_with(&events, &[], utc_date);
        assert_eq!(weeks.len(), 1);
        assert!(weeks[0].patterns.is_empty(), "{:?}", weeks[0].patterns);
    }
    #[test]
    fn top_files_counts_the_lines_each_file_changed_itself() {
        // Both files are touched by both commits, so the ranking comes down to
        // lines: 501 for the big file against 2 for the README. Charging every
        // path the whole commit's total would tie them and sort by name.
        let events = vec![
            commit(
                "2026-09-14T09:00:00Z",
                "one",
                &[("README.md", 1, 0), ("src/big.rs", 500, 0)],
            ),
            commit(
                "2026-09-15T09:00:00Z",
                "two",
                &[("README.md", 1, 0), ("src/big.rs", 1, 0)],
            ),
        ];
        let weeks = bucket_with(&events, &[], utc_date);
        assert_eq!(
            weeks[0].top_files(),
            vec!["src/big.rs".to_string(), "README.md".to_string()]
        );
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
        ];
        let sha = events[0].commit_sha.clone().expect("a commit has a sha");
        let patterns = vec![PatternPayload {
            pattern: PatternKind::FixAfterAi,
            commits: vec![sha],
            detail: "two commits fixed files an AI session wrote".into(),
        }];
        let weeks = bucket_with(&events, &patterns, utc_date);
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
        let weeks = bucket_with(&events, &[], utc_date);
        let text = weeks[0].input_text();
        assert!(text.contains("- and 5 more commits"), "{text}");
        assert!(!text.contains("commit 41"), "{text}");
    }

    #[test]
    fn monday_of_is_idempotent_on_a_monday() {
        assert_eq!(monday_of(date("2026-09-14")), date("2026-09-14"));
        assert_eq!(monday_of(date("2026-09-20")), date("2026-09-14"));
    }

    #[test]
    fn a_backfilled_commit_is_bucketed_by_when_it_was_made() {
        // Three commits from three different weeks in March, all recorded on
        // one afternoon in September.
        let events = [
            backfilled(
                "2026-09-17T12:00:00Z",
                "2026-03-02T09:00:00Z",
                &"a".repeat(40),
                "week one",
            ),
            backfilled(
                "2026-09-17T12:00:01Z",
                "2026-03-10T09:00:00Z",
                &"b".repeat(40),
                "week two",
            ),
            backfilled(
                "2026-09-17T12:00:02Z",
                "2026-03-17T09:00:00Z",
                &"c".repeat(40),
                "week three",
            ),
        ];

        let weeks = bucket_with(&events, &[], utc_date);

        assert_eq!(weeks.len(), 3, "three weeks of work, not one afternoon");
        assert_eq!(weeks[0].week_start(), "2026-03-02");
        assert_eq!(weeks[0].index, 1);
        assert_eq!(weeks[2].week_start(), "2026-03-16");
        assert_eq!(weeks[2].index, 3);
        assert_eq!(weeks[0].commits[0].message, "week one");
    }

    #[test]
    fn a_commit_without_a_commit_time_still_buckets_by_when_it_was_recorded() {
        let events = [commit(
            "2026-09-17T12:00:00Z",
            "no committed_at here",
            &[("a.rs", 1, 0)],
        )];
        let weeks = bucket_with(&events, &[], utc_date);
        assert_eq!(weeks.len(), 1);
        assert_eq!(weeks[0].week_start(), "2026-09-14");
    }

    #[test]
    fn patterns_follow_the_commit_they_are_anchored_to_into_its_real_week() {
        let sha = "a".repeat(40);
        let events = [backfilled(
            "2026-09-17T12:00:00Z",
            "2026-03-02T09:00:00Z",
            &sha,
            "the work",
        )];
        // The analysis ran in September; the pattern belongs to March.
        let patterns = vec![PatternPayload {
            pattern: PatternKind::Revert,
            commits: vec![sha.clone()],
            detail: "undid the parser".into(),
        }];

        let weeks = bucket_with(&events, &patterns, utc_date);

        assert_eq!(weeks.len(), 1, "one week, not two");
        assert_eq!(weeks[0].week_start(), "2026-03-02");
        assert_eq!(weeks[0].patterns.len(), 1);
    }
}
