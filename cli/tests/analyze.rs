//! `madelog analyze` over a real repository: the hook records the commits, the
//! rules read them back, and running twice records nothing new.

mod common;

use common::*;
use madelog::event::{AiSessionFile, AiSessionPayload, EventKind, PatternPayload, Payload};

fn hook(repo: &TestRepo) {
    madelog::run_in(repo.root(), &["hook", "post-commit"]).expect("post-commit hook");
}

fn analyze(repo: &TestRepo) -> String {
    madelog::run_capture(repo.root(), &["analyze"]).expect("madelog analyze")
}

fn analyze_json(repo: &TestRepo) -> String {
    madelog::run_capture(repo.root(), &["analyze", "--json"]).expect("madelog analyze --json")
}

/// Git AI is task B's job; here we put a session in the log by hand so the
/// `fix_after_ai` rule has something to see.
fn record_ai_session(repo: &TestRepo, sha: &str, path: &str, ai_lines: u64, reworked: u64) {
    let store = madelog::store::Store::open(&repo.path(".madelog/log.db")).unwrap();
    store
        .append_payload(
            Some(sha),
            &Payload::AiSession(AiSessionPayload {
                tool: "claude-code".into(),
                model: Some("opus".into()),
                session_id: "session-1".into(),
                prompt: Some("write the parser".into()),
                response_excerpt: None,
                files: vec![AiSessionFile {
                    path: path.into(),
                    ai_lines,
                    ai_lines_modified_by_human: reworked,
                }],
                accepted: true,
            }),
        )
        .unwrap();
}

fn patterns_in(repo: &TestRepo) -> Vec<PatternPayload> {
    events(repo)
        .into_iter()
        .filter(|e| e.kind == EventKind::Pattern)
        .map(|e| e.payload_as::<PatternPayload>().expect("a pattern payload"))
        .collect()
}

fn lines(n: usize, word: &str) -> String {
    (0..n).map(|i| format!("// {word} {i}\n")).collect()
}

/// A history with a fix after AI, a retry run, a revert and a decision that
/// opens a new directory.
fn busy_repo() -> TestRepo {
    let repo = init_repo();
    init_madelog(&repo);

    // 1. the model writes a parser, 2. a human fixes it.
    repo.write("src/parse.rs", &lines(80, "parse"));
    let ai_sha = repo.commit_all("feat: generated parser");
    hook(&repo);
    record_ai_session(&repo, &ai_sha, "src/parse.rs", 80, 0);

    repo.write(
        "src/parse.rs",
        &lines(80, "parse").replace("// parse 0", "// fixed 0"),
    );
    repo.commit_all("fix: the parser was wrong");
    hook(&repo);

    // 3-5. the same file three times with nearly the same message.
    repo.write("src/lex.rs", &lines(12, "lex"));
    repo.commit_all("fix the lexer offset");
    hook(&repo);
    repo.write(
        "src/lex.rs",
        &lines(12, "lex").replace("// lex 3", "// lex three"),
    );
    repo.commit_all("fix the lexer offset again");
    hook(&repo);
    repo.write(
        "src/lex.rs",
        &lines(12, "lex").replace("// lex 4", "// lex four"),
    );
    repo.commit_all("fix the lexer offset properly");
    hook(&repo);

    // 6. and then all of it goes away again.
    std::fs::remove_file(repo.path("src/lex.rs")).unwrap();
    repo.commit_all("Revert the whole lexer experiment");
    hook(&repo);

    // 7. a decision, then a directory nobody had touched.
    madelog::run_capture(repo.root(), &["note", "move the storage behind a trait"]).unwrap();
    repo.write("src/store/mod.rs", &lines(20, "store"));
    repo.write("src/store/sqlite.rs", &lines(30, "sqlite"));
    repo.commit_all("feat: the store module");
    hook(&repo);

    repo
}

#[test]
fn analyze_records_every_pattern_it_finds() {
    let repo = busy_repo();

    let out = analyze(&repo);

    assert!(out.contains("fix_after_ai"), "{out}");
    assert!(out.contains("retry"), "{out}");
    assert!(out.contains("revert"), "{out}");
    assert!(out.contains("recorded 3 new pattern events"), "{out}");

    let recorded = patterns_in(&repo);
    assert_eq!(recorded.len(), 3, "{recorded:?}");
    assert!(recorded
        .iter()
        .all(|p| p.commits.iter().all(|sha| sha.len() == 40)));
    assert!(recorded.iter().all(|p| p.detail.ends_with('.')));

    // Pattern events are anchored to the commit they are about.
    let anchored = events(&repo)
        .into_iter()
        .filter(|e| e.kind == EventKind::Pattern)
        .all(|e| e.commit_sha.is_some());
    assert!(anchored, "every pattern event names its commit");
}

#[test]
fn analyze_prints_the_moments_it_would_put_on_the_page() {
    let repo = busy_repo();

    let out = analyze(&repo);

    assert!(
        out.contains("Fixing AI-written code in src/parse.rs"),
        "{out}"
    );
    assert!(out.contains("(fix_after_ai)"), "{out}");
    assert!(out.contains("A decision, then a new src/store/"), "{out}");
    assert!(out.contains("move the storage behind a trait"), "{out}");
    assert!(out.contains("2 moments for the page:"), "{out}");
}

#[test]
fn analyze_is_idempotent() {
    let repo = busy_repo();

    let first = analyze(&repo);
    let after_first = events(&repo).len();
    let second = analyze(&repo);

    assert_eq!(
        events(&repo).len(),
        after_first,
        "the second run appended events"
    );
    assert!(first.contains("recorded 3 new pattern events"), "{first}");
    assert!(second.contains("nothing new to record"), "{second}");
    assert_eq!(patterns_in(&repo).len(), 3);

    // And the chain still adds up after analysis appended to it.
    let verified = madelog::run_capture(repo.root(), &["verify"]).unwrap();
    assert!(verified.contains("chain ok"), "{verified}");
}

#[test]
fn analyze_json_is_stable_across_runs() {
    let repo = busy_repo();
    analyze(&repo);

    let first = analyze_json(&repo);
    let count = events(&repo).len();
    let second = analyze_json(&repo);

    assert_eq!(first, second, "the same log analyses the same way");
    assert_eq!(events(&repo).len(), count, "--json appended events");

    let parsed: serde_json::Value = serde_json::from_str(&first).unwrap();
    let patterns = parsed["patterns"].as_array().unwrap();
    assert_eq!(patterns.len(), 3);
    assert_eq!(patterns[0]["pattern"], "fix_after_ai");
    assert_eq!(patterns[1]["pattern"], "retry");
    assert_eq!(patterns[2]["pattern"], "revert");

    let moments = parsed["moments"].as_array().unwrap();
    assert_eq!(moments.len(), 2);
    assert_eq!(moments[0]["kind"], "fix_after_ai");
    assert_eq!(moments[1]["kind"], "direction_change");
    assert_eq!(moments[0]["generated"], false);
    assert!(moments[0]["refs"].as_array().unwrap().len() >= 2);
}

#[test]
fn moment_references_point_at_real_events() {
    let repo = busy_repo();
    let parsed: serde_json::Value = serde_json::from_str(&analyze_json(&repo)).unwrap();
    let ids: Vec<String> = events(&repo).into_iter().map(|e| e.id).collect();

    for moment in parsed["moments"].as_array().unwrap() {
        for id in moment["refs"].as_array().unwrap() {
            let id = id.as_str().unwrap().to_string();
            assert!(ids.contains(&id), "{id} is not an event in the log");
        }
    }
}

#[test]
fn a_quiet_repository_has_nothing_to_report() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("README.md", "# a project\n");
    repo.commit_all("docs: readme");
    hook(&repo);

    let out = analyze(&repo);

    assert!(out.contains("no patterns yet"), "{out}");
    assert!(out.contains("no moments yet"), "{out}");
    assert!(patterns_in(&repo).is_empty());
}

#[test]
fn analyze_needs_madelog_init_first() {
    let repo = init_repo();
    isolate_device_key();

    let err = madelog::run_capture(repo.root(), &["analyze"]).unwrap_err();

    assert!(err.to_string().contains("madelog init"), "{err}");
}
