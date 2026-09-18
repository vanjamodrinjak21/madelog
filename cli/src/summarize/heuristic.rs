//! The summary madelog writes when no model is available.
//!
//! Deterministic, plain, and incapable of inventing anything: every sentence
//! is assembled from counts and from text you wrote yourself. The same week
//! always produces the same paragraphs, which is what makes it safe to publish.

use super::weeks::{count, Week};

/// Two or three short paragraphs: what happened, what was decided, what stood
/// out.
pub fn summarize(week: &Week) -> String {
    let mut paragraphs = vec![what_happened(week)];
    let decided = what_was_decided(week);
    let stood_out = what_stood_out(week);
    match (decided, stood_out) {
        (None, None) => paragraphs.push(
            "No decisions were written down that week, and the analysis found no patterns.".into(),
        ),
        (decided, stood_out) => paragraphs.extend(decided.into_iter().chain(stood_out)),
    }
    paragraphs.join("\n\n")
}

fn what_happened(week: &Week) -> String {
    if week.commits.is_empty() {
        return format!(
            "In the week of {} there were no commits.",
            week.week_start()
        );
    }
    let mut sentence = format!(
        "In the week of {} you made {} over {}.",
        week.week_start(),
        count(week.commits.len(), "commit", "commits"),
        count(week.days_active(), "day", "days"),
    );
    let top = week.top_files();
    if !top.is_empty() {
        sentence.push_str(&format!(
            " Most of the changes landed in {}.",
            join_list(&top)
        ));
    }
    sentence
}

fn what_was_decided(week: &Week) -> Option<String> {
    if week.decisions.is_empty() {
        return None;
    }
    let list: Vec<String> = week.decisions.iter().map(|d| trim_stop(d)).collect();
    Some(format!(
        "You wrote down {}: {}.",
        count(week.decisions.len(), "decision", "decisions"),
        list.join("; ")
    ))
}

fn what_stood_out(week: &Week) -> Option<String> {
    if week.patterns.is_empty() {
        return None;
    }
    let list: Vec<String> = week
        .patterns
        .iter()
        .map(|p| {
            let detail = trim_stop(&p.detail);
            if detail.is_empty() {
                label(&p.kind).to_string()
            } else {
                format!("{} ({})", label(&p.kind), detail)
            }
        })
        .collect();
    Some(format!(
        "The analysis found {} that week: {}.",
        count(week.patterns.len(), "pattern", "patterns"),
        list.join("; ")
    ))
}

/// The pattern kinds spelled the way a reader would say them.
fn label(kind: &str) -> &str {
    match kind {
        "revert" => "a revert",
        "retry" => "repeated attempts at the same files",
        "fix_after_ai" => "a fix after an AI session",
        "stall" => "a stall",
        other => other,
    }
}

/// `a, b and c` — no serial comma, because the page is English prose.
fn join_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {}", rest.join(", "), last),
    }
}

/// Your own sentence, minus a full stop we are about to add back.
fn trim_stop(text: &str) -> String {
    text.trim().trim_end_matches('.').trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summarize::weeks::{CommitLine, FileLines, PatternLine};
    use chrono::NaiveDate;

    fn day(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn commit(date: &str, message: &str, paths: &[&str]) -> CommitLine {
        CommitLine {
            date: day(date),
            message: message.into(),
            files: paths.len(),
            added: 10,
            removed: 2,
            paths: paths
                .iter()
                .map(|p| FileLines {
                    path: (*p).to_string(),
                    lines: 1,
                })
                .collect(),
        }
    }

    fn week() -> Week {
        Week {
            index: 1,
            start: day("2026-09-14"),
            end: day("2026-09-20"),
            commits: vec![
                commit("2026-09-14", "add the event store", &["src/store.rs"]),
                commit(
                    "2026-09-15",
                    "chain the events",
                    &["src/store.rs", "src/chain.rs"],
                ),
            ],
            decisions: vec!["chose sqlite over a json file.".into()],
            patterns: vec![PatternLine {
                kind: "fix_after_ai".into(),
                detail: "two commits fixed files an AI session wrote".into(),
            }],
        }
    }

    #[test]
    fn a_full_week_reads_as_three_paragraphs() {
        let text = summarize(&week());
        assert_eq!(
            text,
            "In the week of 2026-09-14 you made 2 commits over 2 days. \
             Most of the changes landed in src/store.rs and src/chain.rs.\n\n\
             You wrote down 1 decision: chose sqlite over a json file.\n\n\
             The analysis found 1 pattern that week: a fix after an AI session \
             (two commits fixed files an AI session wrote)."
        );
        assert_eq!(text.split("\n\n").count(), 3);
    }

    #[test]
    fn the_same_week_always_writes_the_same_words() {
        let week = week();
        assert_eq!(summarize(&week), summarize(&week));
    }

    #[test]
    fn a_quiet_week_still_gets_two_paragraphs() {
        let mut week = week();
        week.decisions.clear();
        week.patterns.clear();
        let text = summarize(&week);
        assert_eq!(text.split("\n\n").count(), 2);
        assert!(
            text.ends_with(
                "No decisions were written down that week, and the analysis found no patterns."
            ),
            "{text}"
        );
    }

    #[test]
    fn singulars_never_read_as_plurals() {
        let mut week = week();
        week.commits.truncate(1);
        let text = summarize(&week);
        assert!(text.starts_with("In the week of 2026-09-14 you made 1 commit over 1 day."));
        assert!(
            text.contains("Most of the changes landed in src/store.rs."),
            "{text}"
        );
    }

    #[test]
    fn a_week_of_notes_without_commits_says_so() {
        let mut week = week();
        week.commits.clear();
        week.patterns.clear();
        let text = summarize(&week);
        assert_eq!(
            text,
            "In the week of 2026-09-14 there were no commits.\n\n\
             You wrote down 1 decision: chose sqlite over a json file."
        );
    }

    #[test]
    fn three_files_are_joined_as_english() {
        assert_eq!(join_list(&["a".into()]), "a");
        assert_eq!(join_list(&["a".into(), "b".into()]), "a and b");
        assert_eq!(
            join_list(&["a".into(), "b".into(), "c".into()]),
            "a, b and c"
        );
    }
}
