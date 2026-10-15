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

#[test]
fn hook_records_when_git_says_the_commit_was_made() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.txt", "one\n");
    // 2026-03-02T09:00:00Z
    repo.commit_all_at("feat: a thing", 1_772_442_000);

    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    let c = commit_payloads(&repo).pop().unwrap();
    assert_eq!(c.committed_at.as_deref(), Some("2026-03-02T09:00:00Z"));

    // `ts` still says when madelog saw it, which is now, not March.
    let ev = events(&repo)
        .into_iter()
        .find(|e| e.kind == EventKind::Commit)
        .unwrap();
    assert!(ev.ts.as_str() > "2026-03-03", "{}", ev.ts);
}

#[test]
fn post_merge_records_the_commit_at_head() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.txt", "one\n");
    let sha = repo.commit_all("merge: bring it in");

    madelog::run_in(repo.root(), &["hook", "post-merge"]).unwrap();

    let commits = commit_payloads(&repo);
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].sha, sha);
    assert!(commits[0].committed_at.is_some());
}

#[test]
fn post_merge_records_a_commit_only_once() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.txt", "one\n");
    repo.commit_all("first");

    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();
    madelog::run_in(repo.root(), &["hook", "post-merge"]).unwrap();

    assert_eq!(
        commit_payloads(&repo).len(),
        1,
        "the hooks do not duplicate"
    );
}

#[test]
fn post_rewrite_records_the_new_shas_it_is_handed_on_stdin() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.txt", "one\n");
    let first = repo.commit_all("first");
    repo.write("a.txt", "one\ntwo\n");
    let second = repo.commit_all("second");

    // git writes `<old> <new>` per rewritten commit.
    let stdin = format!("{} {first}\n{} {second}\n", "0".repeat(40), "1".repeat(40));
    let mut out: Vec<u8> = Vec::new();
    madelog::run_in_with(
        repo.root(),
        &["hook", "post-rewrite"],
        stdin.as_bytes(),
        &mut out,
    )
    .unwrap();

    let recorded: Vec<String> = commit_payloads(&repo).into_iter().map(|c| c.sha).collect();
    assert_eq!(recorded, vec![first, second]);
}

#[test]
fn post_rewrite_skips_commits_that_are_already_recorded() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.txt", "one\n");
    let sha = repo.commit_all("first");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    let stdin = format!("{} {sha}\n", "0".repeat(40));
    let mut out: Vec<u8> = Vec::new();
    madelog::run_in_with(
        repo.root(),
        &["hook", "post-rewrite"],
        stdin.as_bytes(),
        &mut out,
    )
    .unwrap();

    assert_eq!(commit_payloads(&repo).len(), 1);
}

#[test]
fn post_rewrite_with_nothing_on_stdin_does_nothing() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.txt", "one\n");
    repo.commit_all("first");

    let mut out: Vec<u8> = Vec::new();
    madelog::run_in_with(
        repo.root(),
        &["hook", "post-rewrite"],
        std::io::empty(),
        &mut out,
    )
    .unwrap();

    assert!(commit_payloads(&repo).is_empty());
}

#[test]
fn the_new_hooks_never_fail_without_madelog_init() {
    let repo = init_repo();
    isolate_device_key();
    repo.write("a.txt", "one\n");
    repo.commit_all("first");

    madelog::run_in(repo.root(), &["hook", "post-merge"]).expect("post-merge must not fail");
    let mut out: Vec<u8> = Vec::new();
    madelog::run_in_with(
        repo.root(),
        &["hook", "post-rewrite"],
        &b"0000000000000000000000000000000000000000 1111111111111111111111111111111111111111\n"[..],
        &mut out,
    )
    .expect("post-rewrite must not fail");
}

#[test]
fn hook_nudges_when_a_commit_adds_a_dependency() {
    use madelog::commands::hook::dependency_prompt;
    use madelog::gitrepo::Repo;

    let repo = init_repo();
    init_madelog(&repo);
    repo.write("package.json", "{\"dependencies\":{\"a\":\"1\"}}\n");
    let root = repo.commit_all("chore: start");
    let git = Repo::discover(repo.root()).unwrap();
    assert_eq!(
        dependency_prompt(&git, &root),
        None,
        "root commit stays quiet"
    );

    repo.write(
        "package.json",
        "{\"dependencies\":{\"a\":\"1\",\"drizzle-orm\":\"1\",\"better-auth\":\"1\"}}\n",
    );
    let sha = repo.commit_all("feat: storage");
    let line = dependency_prompt(&git, &sha).expect("a nudge");
    assert!(
        line.starts_with("madelog: added better-auth, drizzle-orm. Write down why"),
        "{line}"
    );

    // The hook prints it on stderr, never on stdout.
    let out = madelog::run_capture(repo.root(), &["hook", "post-commit"]).unwrap();
    assert!(!out.contains("Write down why"), "{out}");

    repo.write(
        "package.json",
        "{\"dependencies\":{\"a\":\"1\",\"drizzle-orm\":\"1\",\"better-auth\":\"1\",\"zod\":\"3\"}}\n",
    );
    let sha = repo.commit_all("feat: add zod for validation");
    assert_eq!(dependency_prompt(&git, &sha), None, "message names it");
}
