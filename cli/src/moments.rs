//! Moments: the three to five things worth telling a stranger about a
//! project (spec §4.2).
//!
//! A moment is built from patterns and decisions, never from raw diffs. The
//! ranking is the spec's: a fix after AI first, then a decision that precedes
//! a change of directory structure, then the longest stall and how it
//! resolved. Titles and bodies come from template strings, so a page renders
//! the same with or without a language model; [`MomentCandidate`] is the shape
//! a later Ollama step gets to choose and retitle from.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::event::{DecisionPayload, Event, EventKind, PatternKind, PatternPayload};
use crate::patterns::{
    self, capitalize, files_phrase, humanize_gap, path_phrase, quotable, Commit,
};

/// At most this many moments reach the page.
pub const MAX_MOMENTS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MomentKind {
    FixAfterAi,
    DirectionChange,
    Stall,
}

impl MomentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MomentKind::FixAfterAi => "fix_after_ai",
            MomentKind::DirectionChange => "direction_change",
            MomentKind::Stall => "stall",
        }
    }
}

/// One story from the log, ready to render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Moment {
    pub title: String,
    pub kind: MomentKind,
    /// Two to four sentences.
    pub body: String,
    /// The ids of the events this moment is made of.
    pub refs: Vec<String>,
    /// True only when a language model wrote or retitled it.
    pub generated: bool,
}

/// The candidate list handed to a language model: it may pick and retitle,
/// never invent. Keep this shape stable — it is a prompt contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MomentCandidate {
    /// Position in the candidate list; this is what a model picks by.
    pub index: usize,
    pub kind: MomentKind,
    pub title: String,
    pub body: String,
    pub refs: Vec<String>,
}

/// A model's choice: which candidate, and optionally a better title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MomentChoice {
    pub index: usize,
    #[serde(default)]
    pub title: Option<String>,
}

/// The candidate list for a log, ranked per spec §4.2.
pub fn candidates(events: &[Event], patterns: &[PatternPayload]) -> Vec<Moment> {
    let commits = patterns::commits(events);
    let mut out = Vec::new();
    out.extend(fix_after_ai_moments(&commits, patterns));
    out.extend(direction_changes(events, &commits));
    out.extend(stall_moments(&commits, patterns));
    out
}

/// Three to five moments, or everything there is when there is less.
pub fn pick(candidates: Vec<Moment>) -> Vec<Moment> {
    candidates.into_iter().take(MAX_MOMENTS).collect()
}

/// The JSON a language model is shown.
pub fn candidate_list(candidates: &[Moment]) -> Vec<MomentCandidate> {
    candidates
        .iter()
        .enumerate()
        .map(|(index, m)| MomentCandidate {
            index,
            kind: m.kind,
            title: m.title.clone(),
            body: m.body.clone(),
            refs: m.refs.clone(),
        })
        .collect()
}

/// Apply a model's choices to the candidate list.
///
/// Out-of-range indexes and duplicates are dropped, so a bad answer degrades
/// to fewer moments rather than to wrong ones; an empty result means the
/// caller should fall back to [`pick`].
pub fn apply_choices(candidates: &[Moment], choices: &[MomentChoice]) -> Vec<Moment> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for choice in choices {
        if choice.index >= candidates.len() || !seen.insert(choice.index) {
            continue;
        }
        let mut moment = candidates[choice.index].clone();
        if let Some(title) = choice
            .title
            .as_ref()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
        {
            moment.title = title.to_string();
            moment.generated = true;
        }
        out.push(moment);
        if out.len() == MAX_MOMENTS {
            break;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// fix after AI
// ---------------------------------------------------------------------------

fn fix_after_ai_moments(commits: &[Commit], patterns: &[PatternPayload]) -> Vec<Moment> {
    let mut out = Vec::new();
    for pattern in patterns
        .iter()
        .filter(|p| p.pattern == PatternKind::FixAfterAi)
    {
        let (Some(before), Some(after)) = (
            find(commits, pattern.commits.first()),
            find(commits, pattern.commits.get(1)),
        ) else {
            continue;
        };
        let touched = after.paths();
        let shared: Vec<&str> = before
            .ai_paths()
            .into_iter()
            .filter(|p| touched.contains(p))
            .collect();
        let gap = (after.ts - before.ts).num_seconds();

        let title = match shared.first() {
            Some(path) if shared.len() == 1 => format!("Fixing AI-written code in {path}"),
            _ => "Fixing what the AI wrote".to_string(),
        };

        let mut body = vec![
            format!(
                "{} (\"{}\") carried AI-written lines in {}.",
                before.short(),
                quotable(before.subject(), 60),
                path_phrase(&shared)
            ),
            format!(
                "{} later, {} went back to the same code: \"{}\".",
                capitalize(&humanize_gap(gap)),
                after.short(),
                quotable(after.subject(), 60)
            ),
            format!(
                "{} lines in {} were written by the model.",
                before.ai_lines(),
                before.short()
            ),
        ];
        if after.ai_lines_reworked() > 0 {
            body.push(format!(
                "In {}, {} of the model's lines were changed by hand.",
                after.short(),
                after.ai_lines_reworked()
            ));
        }

        let mut refs = vec![before.event_id.clone()];
        refs.extend(before.ai_event_ids.iter().cloned());
        refs.push(after.event_id.clone());

        out.push(Moment {
            title,
            kind: MomentKind::FixAfterAi,
            body: body.join(" "),
            refs,
            generated: false,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// direction change
// ---------------------------------------------------------------------------

/// A decision whose next commit opens a directory nobody had touched.
fn direction_changes(events: &[Event], commits: &[Commit]) -> Vec<Moment> {
    let mut out = Vec::new();
    let mut next_commit = 0usize;
    for event in events {
        if event.kind == EventKind::Commit {
            next_commit += 1;
            continue;
        }
        if event.kind != EventKind::Decision {
            continue;
        }
        let Ok(decision) = event.payload_as::<DecisionPayload>() else {
            continue;
        };
        if decision.private || decision.text.trim().is_empty() {
            continue;
        }
        let Some(commit) = commits.get(next_commit) else {
            continue;
        };
        let known = dirs_touched_before(commits, next_commit);
        let opened: Vec<&str> = commit
            .files
            .iter()
            .filter(|f| {
                matches!(
                    f.status,
                    crate::event::FileStatus::Added | crate::event::FileStatus::Deleted
                )
            })
            .map(|f| directory_of(&f.path))
            .filter(|d| !known.contains(d))
            .collect();
        let Some(dir) = opened.iter().min().copied() else {
            continue;
        };
        let count = opened.iter().filter(|d| **d == dir).count();
        let removed = opened.len() == count
            && commit
                .files
                .iter()
                .filter(|f| directory_of(&f.path) == dir)
                .all(|f| f.status == crate::event::FileStatus::Deleted);
        let verb = if removed { "removed" } else { "added" };
        let place = if dir.is_empty() {
            "the repository root".to_string()
        } else {
            format!("{dir}/")
        };
        let title = if dir.is_empty() {
            "A decision, then new files at the top level".to_string()
        } else {
            format!("A decision, then a new {dir}/")
        };

        out.push(Moment {
            title,
            kind: MomentKind::DirectionChange,
            body: [
                format!(
                    "You wrote down a decision: \"{}\".",
                    quotable(&decision.text, 120)
                ),
                format!(
                    "The next commit, {}, {} {} under {}.",
                    commit.short(),
                    verb,
                    files_phrase(count),
                    place
                ),
                format!("It was called \"{}\".", quotable(commit.subject(), 60)),
            ]
            .join(" "),
            refs: vec![event.id.clone(), commit.event_id.clone()],
            generated: false,
        });
    }
    out
}

/// Every directory, and every parent of one, touched before commit `upto`.
fn dirs_touched_before(commits: &[Commit], upto: usize) -> BTreeSet<&str> {
    let mut out = BTreeSet::new();
    for commit in commits.iter().take(upto) {
        for file in &commit.files {
            let mut dir = directory_of(&file.path);
            loop {
                out.insert(dir);
                if dir.is_empty() {
                    break;
                }
                dir = directory_of(dir);
            }
        }
    }
    out
}

/// `src/store/mod.rs` → `src/store`; a top-level file → `""`.
fn directory_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

// ---------------------------------------------------------------------------
// stall
// ---------------------------------------------------------------------------

/// Stalls, longest first — the spec asks for the longest one and how it
/// resolved.
fn stall_moments(commits: &[Commit], patterns: &[PatternPayload]) -> Vec<Moment> {
    let mut found: Vec<(i64, Moment)> = Vec::new();
    for pattern in patterns.iter().filter(|p| p.pattern == PatternKind::Stall) {
        let (Some(before), Some(after)) = (
            find(commits, pattern.commits.first()),
            find(commits, pattern.commits.get(1)),
        ) else {
            continue;
        };
        let gap = (after.ts - before.ts).num_seconds();
        let resolution = match commits.iter().find(|c| c.ts > after.ts) {
            Some(next) => format!(
                "The next commit followed {} later.",
                humanize_gap((next.ts - after.ts).num_seconds())
            ),
            None => "That is the last commit recorded.".to_string(),
        };
        found.push((
            gap,
            Moment {
                title: format!(
                    "{} away, then {} at once",
                    capitalize(&humanize_gap(gap)),
                    files_phrase(after.files.len())
                ),
                kind: MomentKind::Stall,
                body: [
                    format!("Nothing was committed for {}.", humanize_gap(gap)),
                    format!(
                        "Work restarted with {} (\"{}\"), touching {} and {} lines.",
                        after.short(),
                        quotable(after.subject(), 60),
                        files_phrase(after.files.len()),
                        after.lines_changed()
                    ),
                    resolution,
                ]
                .join(" "),
                refs: vec![before.event_id.clone(), after.event_id.clone()],
                generated: false,
            },
        ));
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, m)| m).collect()
}

fn find<'a>(commits: &'a [Commit], sha: Option<&String>) -> Option<&'a Commit> {
    let sha = sha?;
    commits.iter().find(|c| &c.sha == sha)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patterns::testdata::*;
    use crate::patterns::{detect, Commit};

    const HOUR: i64 = 3600;
    const DAY: i64 = 86_400;

    fn file_list(n: usize, dir: &str) -> Vec<crate::event::CommitFile> {
        (0..n)
            .map(|i| file(&format!("{dir}/f{i}.rs"), 5, 0))
            .collect()
    }

    /// Sentences end with a full stop followed by a space or the end of the
    /// body; the dot in `src/parse.rs` does not count.
    fn sentences(body: &str) -> usize {
        let chars: Vec<char> = body.chars().collect();
        chars
            .iter()
            .enumerate()
            .filter(|(i, c)| {
                **c == '.' && chars.get(i + 1).map(|n| n.is_whitespace()) != Some(false)
            })
            .count()
    }

    fn moments_of(log: &Log) -> Vec<Moment> {
        candidates(&log.events, &detect(&log.events))
    }

    /// A log with one of each kind of moment in it.
    fn rich_log() -> Log {
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
        log.decision(3 * HOUR, "move the storage behind a trait", false);
        log.commit(
            4 * HOUR,
            "c3d4e5f",
            "feat: the store module",
            file_list(2, "src/store"),
        );
        log.commit(
            9 * DAY,
            "d4e5f60",
            "feat: the new shape",
            file_list(11, "src/page"),
        );
        log.commit(
            9 * DAY + HOUR,
            "e5f6012",
            "chore: tidy",
            vec![file("src/page/f0.rs", 1, 1)],
        );
        log
    }

    #[test]
    fn a_fix_after_ai_pattern_becomes_the_first_moment() {
        let log = rich_log();
        let found = moments_of(&log);
        assert_eq!(found[0].kind, MomentKind::FixAfterAi);
        assert_eq!(found[0].title, "Fixing AI-written code in src/parse.rs");
        assert!(!found[0].generated);
        assert!(
            found[0].body.starts_with(
                "a1b2c3d (\"feat: generated parser\") carried AI-written lines in src/parse.rs."
            ),
            "{}",
            found[0].body
        );
        assert!(
            found[0].body.contains("Two hours later, b2c3d4e went back"),
            "{}",
            found[0].body
        );
        assert!(found[0].body.contains("80 lines in"), "{}", found[0].body);
        // the AI commit, its session, and the fix
        assert_eq!(found[0].refs.len(), 3);
    }

    #[test]
    fn a_decision_before_a_new_directory_is_a_direction_change() {
        let log = rich_log();
        let found = moments_of(&log);
        let m = found
            .iter()
            .find(|m| m.kind == MomentKind::DirectionChange)
            .expect("a direction change");
        assert_eq!(m.title, "A decision, then a new src/store/");
        assert!(
            m.body
                .starts_with("You wrote down a decision: \"move the storage behind a trait\"."),
            "{}",
            m.body
        );
        assert!(
            m.body.contains("c3d4e5f, added 2 files under src/store/"),
            "{}",
            m.body
        );
        assert_eq!(m.refs.len(), 2);
    }

    #[test]
    fn a_decision_followed_by_familiar_directories_is_not_a_direction_change() {
        let mut log = Log::start();
        log.commit(0, "a1b2c3d", "feat: the store", file_list(2, "src/store"));
        log.decision(HOUR, "keep going with the store", false);
        log.commit(
            2 * HOUR,
            "b2c3d4e",
            "feat: more store",
            vec![file("src/store/f0.rs", 4, 2)],
        );

        assert!(moments_of(&log)
            .iter()
            .all(|m| m.kind != MomentKind::DirectionChange));
    }

    #[test]
    fn a_private_decision_is_never_a_moment() {
        let mut log = Log::start();
        log.commit(
            0,
            "a1b2c3d",
            "feat: the beginning",
            vec![file("README.md", 3, 0)],
        );
        log.decision(HOUR, "the client is difficult", true);
        log.commit(
            2 * HOUR,
            "b2c3d4e",
            "feat: the store",
            file_list(2, "src/store"),
        );

        assert!(moments_of(&log).is_empty());
    }

    #[test]
    fn a_decision_with_no_commit_after_it_is_not_a_moment() {
        let mut log = Log::start();
        log.commit(
            0,
            "a1b2c3d",
            "feat: the beginning",
            vec![file("README.md", 3, 0)],
        );
        log.decision(HOUR, "stop here for today", false);

        assert!(moments_of(&log).is_empty());
    }

    #[test]
    fn the_longest_stall_comes_first() {
        let mut log = Log::start();
        log.commit(
            0,
            "a1b2c3d",
            "feat: the beginning",
            vec![file("a.rs", 3, 0)],
        );
        log.commit(
            6 * DAY,
            "b2c3d4e",
            "feat: six days later",
            file_list(10, "src/six"),
        );
        log.commit(
            20 * DAY,
            "c3d4e5f",
            "feat: fourteen days later",
            file_list(10, "src/fourteen"),
        );

        let stalls: Vec<Moment> = moments_of(&log)
            .into_iter()
            .filter(|m| m.kind == MomentKind::Stall)
            .collect();
        assert_eq!(stalls.len(), 2);
        assert_eq!(stalls[0].title, "14 days away, then 10 files at once");
        assert!(
            stalls[0]
                .body
                .starts_with("Nothing was committed for 14 days."),
            "{}",
            stalls[0].body
        );
        assert!(
            stalls[0].body.contains("That is the last commit recorded."),
            "{}",
            stalls[0].body
        );
        assert_eq!(stalls[1].title, "Six days away, then 10 files at once");
        assert!(
            stalls[1]
                .body
                .contains("The next commit followed 14 days later."),
            "{}",
            stalls[1].body
        );
    }

    #[test]
    fn candidates_are_ranked_fix_then_direction_then_stall() {
        let kinds: Vec<MomentKind> = moments_of(&rich_log()).iter().map(|m| m.kind).collect();
        assert_eq!(
            kinds,
            vec![
                MomentKind::FixAfterAi,
                MomentKind::DirectionChange,
                MomentKind::Stall
            ]
        );
    }

    #[test]
    fn every_body_is_two_to_four_sentences() {
        for moment in moments_of(&rich_log()) {
            let n = sentences(&moment.body);
            assert!((2..=4).contains(&n), "{n} sentences in {:?}", moment.body);
        }
    }

    #[test]
    fn pick_takes_at_most_five() {
        let mut log = Log::start();
        log.commit(
            0,
            "a1b2c3d",
            "feat: the beginning",
            vec![file("a.rs", 3, 0)],
        );
        for i in 1..8 {
            log.commit(
                i * 6 * DAY,
                &format!("b2c3d4{i}"),
                "feat: back again",
                file_list(10, &format!("src/d{i}")),
            );
        }
        let all = moments_of(&log);
        assert!(all.len() > MAX_MOMENTS, "{} candidates", all.len());
        assert_eq!(pick(all).len(), MAX_MOMENTS);
    }

    #[test]
    fn pick_keeps_everything_when_there_is_little() {
        let found = moments_of(&rich_log());
        assert_eq!(pick(found.clone()).len(), found.len());
        assert!(pick(Vec::new()).is_empty());
    }

    #[test]
    fn the_candidate_shape_is_stable_json() {
        let found = moments_of(&rich_log());
        let json = serde_json::to_value(candidate_list(&found)).unwrap();
        let first = &json[0];
        assert_eq!(first["index"], 0);
        assert_eq!(first["kind"], "fix_after_ai");
        assert!(first["title"].is_string());
        assert!(first["body"].is_string());
        assert!(first["refs"].is_array());
        assert_eq!(json.as_array().unwrap().len(), found.len());
    }

    #[test]
    fn a_model_may_pick_and_retitle_but_not_invent() {
        let found = moments_of(&rich_log());
        let chosen = apply_choices(
            &found,
            &[
                MomentChoice {
                    index: 2,
                    title: Some("Two weeks off".into()),
                },
                MomentChoice {
                    index: 0,
                    title: None,
                },
                MomentChoice {
                    index: 0,
                    title: Some("again".into()),
                },
                MomentChoice {
                    index: 99,
                    title: Some("invented".into()),
                },
            ],
        );
        assert_eq!(chosen.len(), 2);
        assert_eq!(chosen[0].title, "Two weeks off");
        assert!(chosen[0].generated, "a model title is marked as generated");
        assert_eq!(
            chosen[0].body, found[2].body,
            "the body is never rewritten here"
        );
        assert_eq!(chosen[1].title, found[0].title);
        assert!(!chosen[1].generated);
    }

    #[test]
    fn an_empty_log_has_no_moments() {
        assert!(candidates(&[], &[]).is_empty());
        assert!(pick(candidates(&[], &[])).is_empty());
    }

    #[test]
    fn directories_come_from_paths() {
        assert_eq!(directory_of("src/store/mod.rs"), "src/store");
        assert_eq!(directory_of("README.md"), "");
    }

    #[test]
    fn a_top_level_file_after_a_decision_reads_as_the_repository_root() {
        let mut log = Log::start();
        log.decision(0, "write the readme first", false);
        log.commit(
            HOUR,
            "b2c3d4e",
            "docs: readme",
            vec![file("README.md", 20, 0)],
        );

        let m = moments_of(&log).into_iter().next().expect("a moment");
        assert_eq!(m.title, "A decision, then new files at the top level");
        assert!(m.body.contains("under the repository root."), "{}", m.body);
    }

    #[test]
    fn commits_keep_their_event_ids_for_references() {
        let mut log = Log::start();
        log.commit(
            0,
            "a1b2c3d",
            "feat: the beginning",
            vec![file("a.rs", 1, 0)],
        );
        let commits: Vec<Commit> = patterns::commits(&log.events);
        assert_eq!(commits[0].event_id, "event-1");
    }
}
