//! Automatic mode: what happens without anybody typing a command.
//!
//! One commit should be enough to get an `ai_session` event with real line
//! counts, a pattern pass, a fresh weekly summary and a re-rendered page —
//! and to get all of it silently.

mod common;

use chrono::Utc;
use common::*;
use madelog::event::{AiSessionPayload, EventKind, SummaryPayload};
use madelog::global::GlobalConfig;

fn sessions(repo: &TestRepo) -> Vec<AiSessionPayload> {
    events(repo)
        .into_iter()
        .filter(|e| e.kind == EventKind::AiSession)
        .map(|e| e.payload_as::<AiSessionPayload>().expect("an ai_session"))
        .collect()
}

fn summaries(repo: &TestRepo) -> Vec<SummaryPayload> {
    events(repo)
        .into_iter()
        .filter(|e| e.kind == EventKind::Summary)
        .map(|e| e.payload_as::<SummaryPayload>().expect("a summary"))
        .collect()
}

fn kinds(repo: &TestRepo) -> Vec<String> {
    events(repo)
        .into_iter()
        .map(|e| e.kind.as_str().to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// the hook records the session behind the commit
// ---------------------------------------------------------------------------

#[test]
fn the_hook_attributes_the_lines_the_session_wrote_to_the_commit() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    init_madelog(&repo);
    let _home = fake_claude(&repo, Utc::now());

    let source = fixture_parser_source();
    repo.write("src/parser.rs", &source);
    let sha = repo.commit_all("feat: parse log lines");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    let found = sessions(&repo);
    assert_eq!(found.len(), 1, "{found:#?}");
    let s = &found[0];
    assert_eq!(s.tool, "claude-code");
    assert_eq!(s.session_id, FIXTURE_CLAUDE_SESSION);
    assert_eq!(s.model.as_deref(), Some("claude-sonnet-4-5-20250929"));
    assert_eq!(
        s.prompt.as_deref(),
        Some("write a parser for the log lines")
    );
    assert_eq!(s.response_excerpt, None);
    assert!(s.accepted);

    let file = s.files.iter().find(|f| f.path == "src/parser.rs").unwrap();
    let written = source.lines().filter(|l| !l.trim().is_empty()).count() as u64;
    assert_eq!(
        file.ai_lines, written,
        "every line came straight from the model"
    );
    assert_eq!(file.ai_lines_modified_by_human, 0);

    // The event is anchored to the commit it is about.
    let anchored = events(&repo)
        .into_iter()
        .find(|e| e.kind == EventKind::AiSession)
        .unwrap();
    assert_eq!(anchored.commit_sha.as_deref(), Some(sha.as_str()));
}

#[test]
fn a_line_the_person_changed_before_committing_is_recorded_as_reworked() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    init_madelog(&repo);
    let _home = fake_claude(&repo, Utc::now());

    let source = fixture_parser_source();
    repo.write(
        "src/parser.rs",
        &source.replace("line.trim()", "line.trim_end()"),
    );
    repo.commit_all("feat: parse log lines, my way");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    let file = sessions(&repo)[0]
        .files
        .iter()
        .find(|f| f.path == "src/parser.rs")
        .unwrap()
        .clone();
    let written = source.lines().filter(|l| !l.trim().is_empty()).count() as u64;
    assert_eq!(file.ai_lines, written - 1);
    assert_eq!(file.ai_lines_modified_by_human, 1);
}

#[test]
fn the_hook_records_nothing_about_ai_when_the_intent_said_no() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    isolate_device_key();
    isolate_ai_tools();
    madelog::run_capture(
        repo.root(),
        &[
            "init",
            "--what",
            "a thing",
            "--no-record-ai-sessions",
            "--yes",
        ],
    )
    .unwrap();
    let _home = fake_claude(&repo, Utc::now());

    repo.write("src/parser.rs", &fixture_parser_source());
    repo.commit_all("feat: parse log lines");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    assert!(sessions(&repo).is_empty(), "opting out means opting out");
}

#[test]
fn an_ignored_file_the_session_wrote_never_reaches_the_log() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    repo.write(".gitignore", ".env\n");
    repo.commit_all("chore: ignore the env file");
    init_madelog(&repo);
    let _home = fake_claude(&repo, Utc::now());

    repo.write("src/parser.rs", &fixture_parser_source());
    repo.commit_all("feat: parse log lines");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    let text = format!("{:?}", sessions(&repo));
    assert!(!text.contains(".env"), "{text}");
    assert!(!text.contains("redacted-in-the-fixture"), "{text}");
}

// ---------------------------------------------------------------------------
// the rest of what the hook does
// ---------------------------------------------------------------------------

#[test]
fn the_hook_renders_the_page_and_writes_a_summary_without_saying_a_word() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    init_madelog(&repo);
    let page = repo.path(".madelog/site/index.html");
    let _ = std::fs::remove_file(&page);

    repo.write("a.rs", "// one\n");
    repo.commit_all("feat: the first thing");
    let out = madelog::run_capture(repo.root(), &["hook", "post-commit"]).unwrap();

    assert_eq!(out, "", "a hook that chatters is a hook people delete");
    assert!(page.is_file(), "the page is rendered as the commit lands");
    let html = std::fs::read_to_string(&page).unwrap();
    assert!(html.contains("feat: the first thing"), "and it is current");

    let written = summaries(&repo);
    assert_eq!(written.len(), 1, "{written:#?}");
    assert_eq!(
        written[0].generated_by, "heuristic",
        "a hook never waits on a model that is not already there"
    );
    assert!(!written[0].edited);
}

#[test]
fn the_newest_week_keeps_up_and_the_page_with_it() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    init_madelog(&repo);

    repo.write("a.rs", "// one\n");
    repo.commit_all("feat: the first thing");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    repo.write("b.rs", "// two\n");
    repo.commit_all("feat: the second thing");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    let written = summaries(&repo);
    assert_eq!(written.len(), 2, "the week was written again: {written:#?}");
    assert_eq!(
        written[0].week_start, written[1].week_start,
        "same week, twice"
    );
    assert!(
        written[1].text.contains("2 commits"),
        "the newer one knows about both: {}",
        written[1].text
    );

    let html = std::fs::read_to_string(repo.path(".madelog/site/index.html")).unwrap();
    assert!(html.contains("feat: the second thing"), "the page kept up");
}

#[test]
fn a_summary_you_edited_is_never_written_over() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.rs", "// one\n");
    repo.commit_all("feat: the first thing");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    let id = events(&repo)
        .into_iter()
        .find(|e| e.kind == EventKind::Summary)
        .unwrap()
        .id;
    std::env::set_var("MADELOG_EDITOR", "stdin");
    let mut out: Vec<u8> = Vec::new();
    madelog::run_in_with(
        repo.root(),
        &["summary", "edit", &id],
        &b"I spent the week deciding what to build.\n"[..],
        &mut out,
    )
    .unwrap();
    std::env::remove_var("MADELOG_EDITOR");

    repo.write("b.rs", "// two\n");
    repo.commit_all("feat: the second thing");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    let last = summaries(&repo).pop().unwrap();
    assert!(last.edited, "your words are the last word: {last:#?}");
    assert_eq!(last.generated_by, "user");
}

#[test]
fn a_hook_with_nothing_to_record_leaves_the_page_alone() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.rs", "// one\n");
    repo.commit_all("feat: the first thing");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    let page = repo.path(".madelog/site/index.html");
    std::fs::write(&page, "<!doctype html><html>untouched</html>").unwrap();
    let before = kinds(&repo);

    // The same commit, a second time: git does this after a failed push, a
    // `git commit --amend --no-edit`, a rebase that lands on the same tree.
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    assert_eq!(kinds(&repo), before, "nothing new was appended");
    assert_eq!(
        std::fs::read_to_string(&page).unwrap(),
        "<!doctype html><html>untouched</html>",
        "and nothing was re-rendered"
    );
}

// ---------------------------------------------------------------------------
// init backfills
// ---------------------------------------------------------------------------

#[test]
fn init_records_the_history_the_repository_already_had() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    for (i, message) in ["one", "two", "three"].iter().enumerate() {
        repo.write(&format!("f{i}.rs"), "// x\n");
        repo.commit_all_at(message, 1_772_442_000 + 3600 * i as i64);
    }

    isolate_device_key();
    isolate_ai_tools();
    let out = madelog::run_capture(
        repo.root(),
        &[
            "init",
            "--what",
            "a thing",
            "--for-whom",
            "someone",
            "--yes",
        ],
    )
    .unwrap();

    assert!(out.contains("3 commits"), "init says what it found: {out}");
    assert!(out.contains("recorded 3 commits, oldest first"), "{out}");

    let recorded: Vec<String> = kinds(&repo);
    assert_eq!(
        recorded.iter().filter(|k| *k == "commit").count(),
        3,
        "{recorded:?}"
    );

    let status = madelog::run_capture(repo.root(), &["status"]).unwrap();
    assert!(
        status.contains("every commit on this branch is recorded"),
        "{status}"
    );
}

#[test]
fn init_can_be_told_to_leave_the_history_alone() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    repo.write("a.rs", "// one\n");
    repo.commit_all("first");

    isolate_device_key();
    isolate_ai_tools();
    madelog::run_capture(
        repo.root(),
        &["init", "--what", "a thing", "--no-backfill", "--yes"],
    )
    .unwrap();

    assert_eq!(kinds(&repo), ["intent"]);
}

// ---------------------------------------------------------------------------
// hook auto
// ---------------------------------------------------------------------------

#[test]
fn hook_auto_says_so_once_in_a_repository_madelog_knows_nothing_about() {
    let (_lock, machine) = isolate_machine();
    let repo = init_repo();
    repo.write("a.rs", "// one\n");
    repo.commit_all("first");

    // Nothing was set up, and auto_init was never turned on.
    assert!(!GlobalConfig::load().auto_init);
    let out = madelog::run_capture(repo.root(), &["hook", "auto", "post-commit"]).unwrap();

    assert_eq!(out, "", "the hint goes to stderr, never to stdout");
    assert!(!repo.path(".madelog").exists(), "and nothing was created");

    let hinted = machine.path().join("hinted");
    let markers: Vec<_> = std::fs::read_dir(&hinted).unwrap().flatten().collect();
    assert_eq!(markers.len(), 1, "one marker, so the hint is said once");

    // A second commit does not say it again.
    repo.write("b.rs", "// two\n");
    repo.commit_all("second");
    madelog::run_capture(repo.root(), &["hook", "auto", "post-commit"]).unwrap();
    assert_eq!(std::fs::read_dir(&hinted).unwrap().count(), 1);
}

#[test]
fn hook_auto_sets_a_repository_up_when_you_asked_it_to() {
    let (_lock, _machine) = isolate_machine();
    GlobalConfig {
        auto_init: true,
        record_ai_sessions: true,
    }
    .save()
    .unwrap();

    let repo = init_repo();
    repo.write("a.rs", "// one\n");
    let sha = repo.commit_all("feat: the first thing");

    let out = madelog::run_capture(repo.root(), &["hook", "auto", "post-commit"]).unwrap();

    assert_eq!(out, "", "a commit is not the moment for three questions");
    assert!(repo.path(".madelog/log.db").is_file());
    assert!(repo.path(".madelog/site/index.html").is_file());

    let recorded = events(&repo);
    assert!(recorded.iter().any(|e| e.kind == EventKind::Intent));
    assert!(recorded
        .iter()
        .any(|e| e.kind == EventKind::Commit && e.commit_sha.as_deref() == Some(sha.as_str())));

    // And the intent carries the answer given once, at onboarding.
    let intent: madelog::event::IntentPayload = recorded
        .iter()
        .find(|e| e.kind == EventKind::Intent)
        .unwrap()
        .payload_as()
        .unwrap();
    assert!(intent.record_ai_sessions);
}

#[test]
fn hook_auto_honours_a_no_to_recording_ai_sessions() {
    let (_lock, _machine) = isolate_machine();
    GlobalConfig {
        auto_init: true,
        record_ai_sessions: false,
    }
    .save()
    .unwrap();

    let repo = init_repo();
    let _home = fake_claude(&repo, Utc::now());
    repo.write("src/parser.rs", &fixture_parser_source());
    repo.commit_all("feat: parse log lines");
    madelog::run_capture(repo.root(), &["hook", "auto", "post-commit"]).unwrap();

    let intent: madelog::event::IntentPayload = events(&repo)
        .into_iter()
        .find(|e| e.kind == EventKind::Intent)
        .unwrap()
        .payload_as()
        .unwrap();
    assert!(!intent.record_ai_sessions);
    assert!(sessions(&repo).is_empty());
}

#[test]
fn hook_auto_just_does_the_hook_where_madelog_is_already_set_up() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.rs", "// one\n");
    let sha = repo.commit_all("feat: the first thing");

    madelog::run_capture(repo.root(), &["hook", "auto", "post-commit"]).unwrap();

    assert!(events(&repo)
        .iter()
        .any(|e| e.kind == EventKind::Commit && e.commit_sha.as_deref() == Some(sha.as_str())));
    // One intent, from `init` — `auto` did not write a second one.
    assert_eq!(
        events(&repo)
            .iter()
            .filter(|e| e.kind == EventKind::Intent)
            .count(),
        1
    );
}

#[test]
fn hook_auto_outside_a_repository_does_nothing_at_all() {
    let (_lock, _machine) = isolate_machine();
    let dir = tempfile::TempDir::new().unwrap();
    let out = madelog::run_capture(dir.path(), &["hook", "auto", "post-commit"]).unwrap();
    assert_eq!(out, "");
}

#[test]
fn hook_auto_takes_the_rewritten_commits_off_its_stdin() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.rs", "// one\n");
    repo.commit_all("first");
    madelog::run_in(repo.root(), &["hook", "auto", "post-commit"]).unwrap();

    // Amend it, the way `git commit --amend` does, and hand the pair over.
    repo.write("a.rs", "// one, better\n");
    let amended = amend(&repo, "first, better");
    let pairs = format!("{} {amended}\n", "0".repeat(40));

    let mut out: Vec<u8> = Vec::new();
    madelog::run_in_with(
        repo.root(),
        &["hook", "auto", "post-rewrite"],
        pairs.as_bytes(),
        &mut out,
    )
    .unwrap();

    assert!(events(&repo)
        .iter()
        .any(|e| e.commit_sha.as_deref() == Some(amended.as_str())));
}

/// `git commit --amend`, through git2.
fn amend(repo: &TestRepo, message: &str) -> String {
    let git = git2::Repository::open(repo.root()).unwrap();
    let mut index = git.index().unwrap();
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = git.find_tree(index.write_tree().unwrap()).unwrap();
    let head = git.head().unwrap().peel_to_commit().unwrap();
    head.amend(Some("HEAD"), None, None, None, Some(message), Some(&tree))
        .unwrap()
        .to_string()
}

#[test]
fn the_first_commit_after_a_backfill_still_finds_the_session_behind_it() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    // History from months ago, recorded all at once by `init`.
    for (i, message) in ["one", "two", "three"].iter().enumerate() {
        repo.write(&format!("old{i}.rs"), "// x\n");
        repo.commit_all_at(message, 1_772_442_000 + 3600 * i as i64);
    }
    init_madelog(&repo);
    assert_eq!(
        events(&repo)
            .iter()
            .filter(|e| e.kind == EventKind::Commit)
            .count(),
        3,
        "init backfilled the history"
    );

    // Now a real commit, out of a session that ran twenty minutes ago.
    let _home = fake_claude(&repo, Utc::now() - chrono::Duration::minutes(20));
    repo.write("src/parser.rs", &fixture_parser_source());
    repo.commit_all("feat: parse log lines");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    let found = sessions(&repo);
    assert_eq!(
        found.len(),
        1,
        "the window starts at the previous commit, but never more than a day back: {found:#?}"
    );
    assert!(found[0].accepted);
}

#[test]
fn a_session_that_wrote_nothing_in_this_commit_is_not_attached_to_it() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    init_madelog(&repo);
    let _home = fake_claude(&repo, Utc::now());

    // A commit that has nothing to do with what the session wrote.
    repo.write("README.md", "# a project\n");
    repo.commit_all("docs: a readme");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();
    assert!(sessions(&repo).is_empty());

    // And the commit that does carry its work gets it.
    repo.write("src/parser.rs", &fixture_parser_source());
    repo.commit_all("feat: parse log lines");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();
    assert_eq!(sessions(&repo).len(), 1);
}
