//! `note --amends` and `pin`: the log only grows, the page reads the latest word.

mod common;

use common::*;
use madelog::event::{DecisionPayload, EventKind, PinPayload};
use madelog::page::{self, Verdict};

fn note(repo: &TestRepo, args: &[&str]) {
    let mut a = vec!["note"];
    a.extend_from_slice(args);
    madelog::run_in(repo.root(), &a).unwrap();
}

fn id_of(repo: &TestRepo, text: &str) -> String {
    events(repo)
        .into_iter()
        .find(|e| {
            e.kind == EventKind::Decision
                && e.payload.get("text").and_then(|t| t.as_str()) == Some(text)
        })
        .unwrap_or_else(|| panic!("no decision `{text}`"))
        .id
}

fn repo_with_three() -> TestRepo {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.txt", "one\n");
    repo.commit_all("first");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();
    note(&repo, &["alpha"]);
    note(&repo, &["beta"]);
    note(&repo, &["gamma"]);
    repo
}

fn sheet(repo: &TestRepo) -> Vec<String> {
    let ctx = page::build_context(repo.root(), None).unwrap();
    Verdict::of(&ctx)
        .sheet_decisions
        .into_iter()
        .map(|d| d.text)
        .collect()
}

fn highlights(repo: &TestRepo) -> Vec<String> {
    let ctx = page::build_context(repo.root(), None).unwrap();
    Verdict::of(&ctx).highlights
}

fn err(repo: &TestRepo, args: &[&str]) -> String {
    format!("{:#}", madelog::run_in(repo.root(), args).unwrap_err())
}

#[test]
fn an_amendment_replaces_the_decision_in_place_and_the_chain_still_verifies() {
    let repo = repo_with_three();
    let beta = id_of(&repo, "beta");
    note(&repo, &["--amends", &beta, "beta, corrected"]);

    let ctx = page::build_context(repo.root(), None).unwrap();
    let titles: Vec<&str> = ctx
        .timeline
        .iter()
        .filter(|e| e.kind == "decision")
        .map(|e| e.title.as_str())
        .collect();
    assert_eq!(titles, ["alpha", "beta, corrected", "gamma"]);
    let entry = ctx.timeline.iter().find(|e| e.id == beta).unwrap();
    assert_eq!(entry.title, "beta, corrected", "original id and position");
    assert_eq!(ctx.stats.decisions, 3, "counted once");

    let html = page::render(&ctx).unwrap();
    assert!(html.contains("beta, corrected"));
    assert!(!html.contains(">beta<"));

    let out = madelog::run_capture(repo.root(), &["export", "--report"]).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let texts: Vec<&str> = v["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["alpha", "beta, corrected", "gamma"]);
    assert_eq!(v["stats"]["decisions"], 3);

    madelog::run_capture(repo.root(), &["verify"]).unwrap();
    // The original is still in the chain, unedited.
    assert!(events(&repo)
        .iter()
        .any(|e| e.payload.get("text").and_then(|t| t.as_str()) == Some("beta")));
}

#[test]
fn a_chain_of_amendments_resolves_to_the_last_and_accepts_a_short_id() {
    let repo = repo_with_three();
    let alpha = id_of(&repo, "alpha");
    note(&repo, &["--amends", &alpha[..13], "alpha v2"]);
    let v2 = id_of(&repo, "alpha v2");
    note(&repo, &["--amends", &v2, "alpha v3"]);

    let ctx = page::build_context(repo.root(), None).unwrap();
    let titles: Vec<&str> = ctx
        .timeline
        .iter()
        .filter(|e| e.kind == "decision")
        .map(|e| e.title.as_str())
        .collect();
    assert_eq!(titles, ["alpha v3", "beta", "gamma"]);
    assert_eq!(ctx.stats.decisions, 3);
}

#[test]
fn the_privacy_of_the_newest_amendment_wins() {
    let repo = repo_with_three();
    let beta = id_of(&repo, "beta");
    note(
        &repo,
        &["--amends", &beta, "--private", "beta, not for the page"],
    );

    let ctx = page::build_context(repo.root(), None).unwrap();
    assert_eq!(ctx.stats.decisions, 2);
    let html = page::render(&ctx).unwrap();
    assert!(!html.contains("not for the page"));
    assert_eq!(sheet(&repo), ["alpha", "gamma"]);
}

#[test]
fn amending_needs_an_existing_decision() {
    let repo = repo_with_three();
    let commit = events(&repo)
        .into_iter()
        .find(|e| e.kind == EventKind::Commit)
        .unwrap();
    assert!(err(&repo, &["note", "--amends", "nope", "x"]).contains("no decision"));
    assert!(err(&repo, &["note", "--amends", &commit.id, "x"]).contains("not a decision"));
    // Every decision id starts with the same timestamp digits: too short to be unique.
    assert!(err(&repo, &["note", "--amends", "0", "x"]).contains("matches"));
    let decisions = events(&repo)
        .iter()
        .filter(|e| e.kind == EventKind::Decision)
        .count();
    assert_eq!(decisions, 3, "nothing was recorded");
}

#[test]
fn pin_puts_decisions_first_in_pin_order_and_leads_the_headline() {
    let repo = repo_with_three();
    assert_eq!(sheet(&repo), ["alpha", "beta", "gamma"]);
    assert!(highlights(&repo).contains(&"Decided: gamma".to_string()));

    madelog::run_in(repo.root(), &["pin", &id_of(&repo, "gamma")]).unwrap();
    madelog::run_in(repo.root(), &["pin", &id_of(&repo, "beta")]).unwrap();
    assert_eq!(sheet(&repo), ["gamma", "beta", "alpha"]);
    assert!(highlights(&repo).contains(&"Decided: gamma".to_string()));

    // Unpinning the first makes beta the headline.
    madelog::run_in(repo.root(), &["pin", "--remove", &id_of(&repo, "gamma")]).unwrap();
    assert_eq!(sheet(&repo), ["beta", "alpha", "gamma"]);
    assert!(highlights(&repo).contains(&"Decided: beta".to_string()));

    madelog::run_in(repo.root(), &["pin", "--remove", &id_of(&repo, "beta")]).unwrap();
    assert_eq!(sheet(&repo), ["alpha", "beta", "gamma"]);
    assert!(highlights(&repo).contains(&"Decided: gamma".to_string()));
    madelog::run_capture(repo.root(), &["verify"]).unwrap();
}

#[test]
fn a_pin_follows_the_amended_decision() {
    let repo = repo_with_three();
    let beta = id_of(&repo, "beta");
    madelog::run_in(repo.root(), &["pin", &beta]).unwrap();
    note(&repo, &["--amends", &beta, "beta, corrected"]);
    assert_eq!(sheet(&repo), ["beta, corrected", "alpha", "gamma"]);
}

#[test]
fn pin_is_recorded_idempotent_and_readable_in_the_log() {
    let repo = repo_with_three();
    let gamma = id_of(&repo, "gamma");
    madelog::run_in(repo.root(), &["pin", &gamma]).unwrap();
    let said = madelog::run_capture(repo.root(), &["pin", &gamma]).unwrap();
    assert!(said.contains("already pinned"), "{said}");
    let pins: Vec<PinPayload> = events(&repo)
        .iter()
        .filter(|e| e.kind == EventKind::Pin)
        .map(|e| e.payload_as().unwrap())
        .collect();
    assert_eq!(
        pins,
        [PinPayload {
            target_id: gamma.clone(),
            pinned: true
        }]
    );
    let log = madelog::run_capture(repo.root(), &["log"]).unwrap();
    assert!(log.contains(&format!("pinned {gamma}")), "{log}");

    note(&repo, &["--amends", &gamma, "gamma, corrected"]);
    let log = madelog::run_capture(repo.root(), &["log"]).unwrap();
    assert!(log.contains(&format!("(amends {gamma})")), "{log}");
}

#[test]
fn pin_refuses_private_unknown_and_non_decisions() {
    let repo = repo_with_three();
    note(&repo, &["--private", "secret"]);
    assert!(err(&repo, &["pin", &id_of(&repo, "secret")]).contains("private"));
    assert!(err(&repo, &["pin", "nope"]).contains("no decision"));
    let commit = events(&repo)
        .into_iter()
        .find(|e| e.kind == EventKind::Commit)
        .unwrap();
    assert!(err(&repo, &["pin", &commit.id]).contains("not a decision"));
    assert!(events(&repo).iter().all(|e| e.kind != EventKind::Pin));
}

#[test]
fn an_old_decision_payload_serialises_without_amends() {
    let d = DecisionPayload {
        text: "t".into(),
        private: false,
        amends: None,
    };
    assert!(serde_json::to_value(d).unwrap().get("amends").is_none());
}
