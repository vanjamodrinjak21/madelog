mod common;

use common::*;
use madelog::event::{DecisionPayload, EventKind};

#[test]
fn note_appends_a_decision_anchored_to_head() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("src/main.rs", "fn main() {}\n");
    let sha = repo.commit_all("first commit");

    madelog::run_in(repo.root(), &["note", "chose sqlite over a json file"]).unwrap();

    let evs = events(&repo);
    let last = evs.last().unwrap();
    assert_eq!(last.kind, EventKind::Decision);
    assert_eq!(last.commit_sha.as_deref(), Some(sha.as_str()));

    let d: DecisionPayload = serde_json::from_value(last.payload.clone()).unwrap();
    assert_eq!(d.text, "chose sqlite over a json file");
    assert!(!d.private);
}

#[test]
fn note_without_any_commit_has_no_commit_sha() {
    let repo = init_repo();
    init_madelog(&repo);

    madelog::run_in(repo.root(), &["note", "nothing committed yet"]).unwrap();

    let evs = events(&repo);
    let last = evs.last().unwrap();
    assert_eq!(last.kind, EventKind::Decision);
    assert_eq!(last.commit_sha, None);
}

#[test]
fn note_private_flag_is_recorded() {
    let repo = init_repo();
    init_madelog(&repo);

    madelog::run_in(repo.root(), &["note", "--private", "burned out this week"]).unwrap();

    let evs = events(&repo);
    let d: DecisionPayload = serde_json::from_value(evs.last().unwrap().payload.clone()).unwrap();
    assert!(d.private);
    assert_eq!(d.text, "burned out this week");
}

#[test]
fn note_requires_madelog_init() {
    let repo = init_repo();
    isolate_device_key();
    let err = madelog::run_in(repo.root(), &["note", "too early"]).unwrap_err();
    assert!(format!("{err:#}").contains("madelog init"), "{err:#}");
}

#[test]
fn note_at_anchors_to_the_commit_you_name() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.rs", "// a\n");
    let first = repo.commit_all("first commit");
    repo.write("b.rs", "// b\n");
    repo.commit_all("second commit");

    // A short sha is what a person actually has to hand.
    madelog::run_in(repo.root(), &["note", "--at", &first[..7], "chose sqlite"]).unwrap();

    let evs = events(&repo);
    let last = evs.last().unwrap();
    assert_eq!(last.kind, EventKind::Decision);
    assert_eq!(
        last.commit_sha.as_deref(),
        Some(first.as_str()),
        "anchored to the named commit, not HEAD"
    );
}

#[test]
fn note_at_prints_the_commit_it_anchored_to() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.rs", "// a\n");
    let sha = repo.commit_all("first commit");

    let out = madelog::run_capture(repo.root(), &["note", "--at", &sha, "chose sqlite"]).unwrap();
    assert!(out.contains(&sha[..7]), "{out}");
}

#[test]
fn note_at_an_unknown_commit_is_an_error() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.rs", "// a\n");
    repo.commit_all("first commit");

    let err = madelog::run_in(repo.root(), &["note", "--at", "deadbee", "too clever"]).unwrap_err();
    assert!(format!("{err:#}").contains("deadbee"), "{err:#}");
    assert!(
        !events(&repo).iter().any(|e| e.kind == EventKind::Decision),
        "nothing is recorded when the anchor is wrong"
    );
}
