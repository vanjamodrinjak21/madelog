//! `madelog summarize` and `madelog summary edit`, end to end, with `llm.provider =
//! none` so the tests never look for a model.

mod common;

use common::*;
use madelog::event::{
    CommitFile, CommitPayload, DecisionPayload, EventKind, FileStatus, Payload, SummaryPayload,
};
use madelog::store::Store;

/// Midday UTC on a Wednesday: no timezone on earth moves that out of its week.
const WEEK_ONE: &str = "2026-01-07T12:00:00.000Z";
const WEEK_THREE: &str = "2026-01-21T12:00:00.000Z";

fn store(repo: &TestRepo) -> Store {
    Store::open(&repo.path(".madelog/log.db")).unwrap()
}

/// Append a commit event with a timestamp of our choosing. The post-commit
/// hook can only ever record "now", and a week is a thing that takes a week.
fn record_commit(repo: &TestRepo, ts: &str, sha: &str, message: &str, path: &str) {
    let payload = Payload::Commit(CommitPayload {
        sha: sha.to_string(),
        message: message.to_string(),
        author: "Test Person".into(),
        parents: vec![],
        files: vec![CommitFile {
            path: path.to_string(),
            added: 12,
            removed: 3,
            status: FileStatus::Modified,
            diff_excerpt: Some("+ let secret = \"hunter2\";".into()),
        }],
        secs_since_prev: None,
        committed_at: None,
    });
    store(repo)
        .append_at(
            ts.to_string(),
            EventKind::Commit,
            Some(sha),
            payload.to_value(),
        )
        .unwrap();
}

fn record_decision(repo: &TestRepo, ts: &str, text: &str, private: bool) {
    let payload = Payload::Decision(DecisionPayload {
        text: text.to_string(),
        private,
        amends: None,
    });
    store(repo)
        .append_at(
            ts.to_string(),
            EventKind::Decision,
            None,
            payload.to_value(),
        )
        .unwrap();
}

/// A repo with two weeks of history and no language model in sight.
fn two_weeks() -> TestRepo {
    let repo = init_repo();
    init_madelog(&repo);
    madelog::run_capture(repo.root(), &["config", "set", "llm.provider", "none"]).unwrap();

    record_commit(
        &repo,
        WEEK_ONE,
        &"a".repeat(40),
        "add the store",
        "src/store.rs",
    );
    record_decision(&repo, WEEK_ONE, "chose sqlite over a json file", false);
    record_decision(&repo, WEEK_ONE, "burned out on wednesday", true);
    record_commit(
        &repo,
        WEEK_THREE,
        &"b".repeat(40),
        "render the page",
        "src/page.rs",
    );
    repo
}

fn summaries(repo: &TestRepo) -> Vec<SummaryPayload> {
    events(repo)
        .iter()
        .filter(|e| e.kind == EventKind::Summary)
        .map(|e| serde_json::from_value(e.payload.clone()).unwrap())
        .collect()
}

#[test]
fn summarize_writes_one_summary_per_week() {
    let repo = two_weeks();

    let out = madelog::run_capture(repo.root(), &["summarize"]).unwrap();

    let written = summaries(&repo);
    assert_eq!(written.len(), 2, "{out}");
    assert_eq!(written[0].week_start, "2026-01-05");
    assert_eq!(written[0].week_end, "2026-01-11");
    assert_eq!(written[1].week_start, "2026-01-19");
    assert_eq!(written[1].week_end, "2026-01-25");
    for summary in &written {
        assert_eq!(summary.generated_by, "heuristic");
        assert!(!summary.edited);
        assert!(!summary.text.trim().is_empty());
    }

    // Every summary is printed, labelled, and addressable by its event id.
    assert_eq!(out.matches("(generated)").count(), 2, "{out}");
    for event in events(&repo)
        .iter()
        .filter(|e| e.kind == EventKind::Summary)
    {
        assert!(out.contains(&event.id), "{out}");
    }
    assert!(out.contains("week 1  2026-01-05 to 2026-01-11"), "{out}");
    assert!(out.contains("week 3  2026-01-19 to 2026-01-25"), "{out}");
    assert!(out.contains("chose sqlite over a json file"), "{out}");

    // A summary is text about your work, never your work itself.
    assert!(!out.contains("hunter2"), "{out}");
    assert!(!out.contains("burned out"), "{out}");

    madelog::run_capture(repo.root(), &["verify"]).expect("the chain still verifies");
}

#[test]
fn summarize_twice_writes_nothing_the_second_time() {
    let repo = two_weeks();
    madelog::run_capture(repo.root(), &["summarize"]).unwrap();
    let after_first = events(&repo).len();

    let out = madelog::run_capture(repo.root(), &["summarize"]).unwrap();

    assert_eq!(events(&repo).len(), after_first, "{out}");
    assert!(out.contains("--force"), "{out}");
}

#[test]
fn force_writes_a_new_summary_over_the_old_one() {
    let repo = two_weeks();
    madelog::run_capture(repo.root(), &["summarize"]).unwrap();

    madelog::run_capture(repo.root(), &["summarize", "--force"]).unwrap();

    // Four events in the log, two weeks on the page: summaries are appended,
    // never rewritten.
    assert_eq!(summaries(&repo).len(), 4);
    let latest = madelog::summarize::latest_summaries(&events(&repo));
    assert_eq!(latest.len(), 2);
    assert_eq!(latest[0].week_start, "2026-01-05");
    assert_eq!(latest[1].week_start, "2026-01-19");
}

#[test]
fn a_single_week_can_be_rebuilt_on_its_own() {
    let repo = two_weeks();
    madelog::run_capture(repo.root(), &["summarize"]).unwrap();

    let out = madelog::run_capture(repo.root(), &["summarize", "--week", "3", "--force"]).unwrap();

    let written = summaries(&repo);
    assert_eq!(written.len(), 3);
    assert_eq!(written[2].week_start, "2026-01-19");
    assert!(out.contains("week 3"), "{out}");
    assert!(!out.contains("week 1  "), "{out}");
}

#[test]
fn an_unknown_week_says_which_weeks_exist() {
    let repo = two_weeks();
    let err = madelog::run_in(repo.root(), &["summarize", "--week", "9"]).unwrap_err();
    assert!(format!("{err:#}").contains("no week 9"), "{err:#}");
    assert!(format!("{err:#}").contains("1, 3"), "{err:#}");
}

#[test]
fn a_repository_without_commits_has_nothing_to_summarise() {
    let repo = init_repo();
    init_madelog(&repo);
    madelog::run_capture(repo.root(), &["config", "set", "llm.provider", "none"]).unwrap();

    let out = madelog::run_capture(repo.root(), &["summarize"]).unwrap();

    assert!(out.contains("nothing to summarise yet"), "{out}");
    assert!(summaries(&repo).is_empty());
}

#[test]
fn summary_edit_appends_your_own_words() {
    std::env::set_var("MADELOG_EDITOR", "stdin");
    let repo = two_weeks();
    madelog::run_capture(repo.root(), &["summarize"]).unwrap();

    let generated = events(&repo)
        .into_iter()
        .find(|e| e.kind == EventKind::Summary)
        .unwrap();

    let mut out: Vec<u8> = Vec::new();
    madelog::run_in_with(
        repo.root(),
        &["summary", "edit", &generated.id],
        "That week I replaced the json file with sqlite.".as_bytes(),
        &mut out,
    )
    .unwrap();
    let out = String::from_utf8(out).unwrap();

    let written = summaries(&repo);
    assert_eq!(written.len(), 3);
    let edited = written.last().unwrap();
    assert_eq!(edited.generated_by, "user");
    assert!(edited.edited);
    assert_eq!(
        edited.text,
        "That week I replaced the json file with sqlite."
    );
    assert_eq!(edited.week_start, "2026-01-05");
    assert_eq!(edited.week_end, "2026-01-11");
    assert!(out.contains("edited by you"), "{out}");

    // The page shows the last summary per week, which is now yours.
    let latest = madelog::summarize::latest_summaries(&events(&repo));
    assert_eq!(latest.len(), 2);
    assert_eq!(
        latest[0].text,
        "That week I replaced the json file with sqlite."
    );
    assert!(latest[0].edited);

    madelog::run_capture(repo.root(), &["verify"]).expect("the chain still verifies");
}

#[test]
fn an_edit_that_changes_nothing_appends_nothing() {
    std::env::set_var("MADELOG_EDITOR", "stdin");
    let repo = two_weeks();
    madelog::run_capture(repo.root(), &["summarize"]).unwrap();
    let generated = events(&repo)
        .into_iter()
        .find(|e| e.kind == EventKind::Summary)
        .unwrap();
    let before = events(&repo).len();
    let text: SummaryPayload = serde_json::from_value(generated.payload.clone()).unwrap();

    let mut out: Vec<u8> = Vec::new();
    madelog::run_in_with(
        repo.root(),
        &["summary", "edit", &generated.id],
        text.text.as_bytes(),
        &mut out,
    )
    .unwrap();

    assert_eq!(events(&repo).len(), before);
    assert!(String::from_utf8(out).unwrap().contains("unchanged"));
}

#[test]
fn an_empty_edit_is_refused() {
    std::env::set_var("MADELOG_EDITOR", "stdin");
    let repo = two_weeks();
    madelog::run_capture(repo.root(), &["summarize"]).unwrap();
    let generated = events(&repo)
        .into_iter()
        .find(|e| e.kind == EventKind::Summary)
        .unwrap();

    let mut out: Vec<u8> = Vec::new();
    let err = madelog::run_in_with(
        repo.root(),
        &["summary", "edit", &generated.id],
        "   \n".as_bytes(),
        &mut out,
    )
    .unwrap_err();

    assert!(format!("{err:#}").contains("cannot be empty"), "{err:#}");
    assert_eq!(summaries(&repo).len(), 2);
}

#[test]
fn summary_edit_refuses_anything_that_is_not_a_summary() {
    let repo = two_weeks();
    let decision = events(&repo)
        .into_iter()
        .find(|e| e.kind == EventKind::Decision)
        .unwrap();

    let err = madelog::run_in(repo.root(), &["summary", "edit", &decision.id]).unwrap_err();
    assert!(format!("{err:#}").contains("is a decision"), "{err:#}");

    let err = madelog::run_in(repo.root(), &["summary", "edit", "not-an-id"]).unwrap_err();
    assert!(format!("{err:#}").contains("no event with id"), "{err:#}");
}

#[test]
fn a_remote_model_is_refused_and_the_template_writes_instead() {
    let repo = two_weeks();
    madelog::run_capture(repo.root(), &["config", "set", "llm.remote", "true"]).unwrap();

    let out = madelog::run_capture(repo.root(), &["summarize"]).unwrap();

    assert!(
        out.contains("remote models are not supported in this version"),
        "{out}"
    );
    for summary in summaries(&repo) {
        assert_eq!(summary.generated_by, "heuristic");
    }
}
