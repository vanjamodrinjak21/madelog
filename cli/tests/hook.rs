mod common;

use common::*;
use madelog::event::{CommitPayload, EventKind, FileStatus};

fn commit_payloads(repo: &TestRepo) -> Vec<CommitPayload> {
    events(repo)
        .into_iter()
        .filter(|e| e.kind == EventKind::Commit)
        .map(|e| serde_json::from_value(e.payload).unwrap())
        .collect()
}

#[test]
fn hook_records_a_commit_with_stats_excerpt_and_no_previous_gap() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("src/main.rs", "fn main() {\n    println!(\"one\");\n}\n");
    let sha = repo.commit_all("feat: first light");

    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    let commits = commit_payloads(&repo);
    assert_eq!(commits.len(), 1);
    let c = &commits[0];
    assert_eq!(c.sha, sha);
    assert_eq!(c.message.trim(), "feat: first light");
    assert!(c.author.contains("Test Person"), "{}", c.author);
    assert!(c.parents.is_empty());
    assert_eq!(c.secs_since_prev, None, "first commit has no previous gap");

    assert_eq!(c.files.len(), 1);
    let f = &c.files[0];
    assert_eq!(f.path, "src/main.rs");
    assert_eq!(f.status, FileStatus::Added);
    assert_eq!(f.added, 3);
    assert_eq!(f.removed, 0);
    let excerpt = f.diff_excerpt.as_deref().expect("excerpt present");
    assert!(excerpt.contains("fn main"), "{excerpt}");
    assert!(excerpt.lines().count() <= 20, "at most 20 lines: {excerpt}");

    // The event is anchored to the commit.
    let ev = events(&repo)
        .into_iter()
        .find(|e| e.kind == EventKind::Commit)
        .unwrap();
    assert_eq!(ev.commit_sha.as_deref(), Some(sha.as_str()));
}

#[test]
fn hook_records_seconds_since_the_previous_commit_event() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.txt", "one\n");
    repo.commit_all("first");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    repo.write("a.txt", "one\ntwo\n");
    repo.commit_all("second");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    let commits = commit_payloads(&repo);
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].secs_since_prev, None);
    let gap = commits[1].secs_since_prev.expect("second commit has a gap");
    assert!(gap >= 0, "gap was {gap}");
    assert_eq!(commits[1].files[0].status, FileStatus::Modified);
    assert_eq!(commits[1].files[0].added, 1);
    assert_eq!(commits[1].parents.len(), 1);
}

#[test]
fn hook_records_each_commit_only_once() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.txt", "one\n");
    repo.commit_all("first");

    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    assert_eq!(commit_payloads(&repo).len(), 1);
}

#[test]
fn hook_omits_excerpts_for_ignored_and_redacted_paths() {
    let repo = init_repo();
    init_madelog(&repo);
    madelog::run_in(
        repo.root(),
        &["config", "set", "privacy.redact", "^src/secret\\.rs$"],
    )
    .unwrap();

    repo.write(".gitignore", "secrets/\n");
    repo.write("src/ok.rs", "pub fn ok() -> u8 { 1 }\n");
    repo.write("src/secret.rs", "const TOKEN: &str = \"sk-live-abc\";\n");
    repo.write("secrets/token.txt", "sk-live-abc\n");
    repo.commit_all_forcing("chore: add sources", &["secrets/token.txt"]);

    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    let c = commit_payloads(&repo).pop().unwrap();
    let by = |p: &str| {
        c.files
            .iter()
            .find(|f| f.path == p)
            .unwrap_or_else(|| panic!("{p} missing from {:?}", c.files))
    };

    assert!(
        by("src/ok.rs").diff_excerpt.is_some(),
        "normal file keeps its excerpt"
    );
    assert!(
        by("src/secret.rs").diff_excerpt.is_none(),
        "redacted path has no excerpt"
    );
    assert!(
        by("secrets/token.txt").diff_excerpt.is_none(),
        "git-ignored path has no excerpt"
    );
    // Stats still recorded for every file.
    assert_eq!(by("src/secret.rs").added, 1);
    assert_eq!(by("secrets/token.txt").added, 1);

    // And nothing secret leaked into the stored payload.
    let raw = serde_json::to_string(&c).unwrap();
    assert!(!raw.contains("sk-live-abc"), "{raw}");
}

#[test]
fn hook_omits_excerpts_for_binary_files() {
    let repo = init_repo();
    init_madelog(&repo);
    std::fs::write(repo.path("logo.png"), [0u8, 1, 2, 0, 3, 255, 0, 9]).unwrap();
    repo.commit_all("chore: add logo");

    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    let c = commit_payloads(&repo).pop().unwrap();
    let f = c.files.iter().find(|f| f.path == "logo.png").unwrap();
    assert!(f.diff_excerpt.is_none(), "binary file has no excerpt");
}

#[test]
fn hook_truncates_long_excerpts_to_twenty_lines() {
    let repo = init_repo();
    init_madelog(&repo);
    let big: String = (0..200).map(|i| format!("line {i}\n")).collect();
    repo.write("big.txt", &big);
    repo.commit_all("feat: big file");

    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    let c = commit_payloads(&repo).pop().unwrap();
    let excerpt = c.files[0].diff_excerpt.as_deref().unwrap();
    assert_eq!(excerpt.lines().count(), 20, "{excerpt}");
}

#[test]
fn hook_records_no_ai_sessions_until_the_git_ai_reader_lands() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.txt", "one\n");
    repo.commit_all("first");

    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    assert!(!events(&repo).iter().any(|e| e.kind == EventKind::AiSession));
}

#[test]
fn hook_never_fails_even_without_madelog_init() {
    let repo = init_repo();
    isolate_device_key();
    repo.write("a.txt", "one\n");
    repo.commit_all("first");

    madelog::run_in(repo.root(), &["hook", "post-commit"]).expect("hook must not fail the commit");
    assert!(!repo.path(".madelog/log.db").exists());
}
