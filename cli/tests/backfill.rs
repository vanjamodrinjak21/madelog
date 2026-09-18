//! `madelog backfill` — the history a repository already had when madelog
//! arrived.

mod common;

use common::*;
use madelog::event::{CommitPayload, EventKind};

/// 2026-03-02T09:00:00Z, and one hour per commit after it.
const MARCH: i64 = 1_772_442_000;

fn commit_payloads(repo: &TestRepo) -> Vec<CommitPayload> {
    events(repo)
        .into_iter()
        .filter(|e| e.kind == EventKind::Commit)
        .map(|e| serde_json::from_value(e.payload).unwrap())
        .collect()
}

/// Three commits made in March, before madelog was anywhere near this repo.
fn repo_with_history() -> (TestRepo, Vec<String>) {
    let repo = init_repo();
    let mut shas = Vec::new();
    for (i, message) in ["feat: the parser", "feat: the store", "fix: the parser"]
        .iter()
        .enumerate()
    {
        repo.write(&format!("src/f{i}.rs"), &format!("fn f{i}() {{}}\n"));
        shas.push(repo.commit_all_at(message, MARCH + 3600 * i as i64));
    }
    (repo, shas)
}

#[test]
fn backfill_records_existing_history_oldest_first_with_real_commit_times() {
    let (repo, shas) = repo_with_history();
    init_madelog(&repo);

    let out = madelog::run_capture(repo.root(), &["backfill", "--yes"]).unwrap();

    let commits = commit_payloads(&repo);
    assert_eq!(commits.len(), 3, "{out}");
    let recorded: Vec<&str> = commits.iter().map(|c| c.sha.as_str()).collect();
    assert_eq!(
        recorded, shas,
        "oldest first, in the order the work happened"
    );

    assert_eq!(
        commits[0].committed_at.as_deref(),
        Some("2026-03-02T09:00:00Z"),
        "the real commit time, not the moment backfill ran"
    );
    assert_eq!(
        commits[2].committed_at.as_deref(),
        Some("2026-03-02T11:00:00Z")
    );

    // The chain still says when madelog saw them, which is now.
    let ts = events(&repo)
        .into_iter()
        .find(|e| e.kind == EventKind::Commit)
        .unwrap()
        .ts;
    assert!(
        ts.as_str() > "2026-03-03",
        "recorded now, not in March: {ts}"
    );

    // Real stats and excerpts, exactly as the hook would have written them.
    assert_eq!(commits[0].files.len(), 1);
    assert_eq!(commits[0].files[0].path, "src/f0.rs");
    assert!(commits[0].files[0].diff_excerpt.is_some());
    assert!(commits[0].author.contains("Test Person"));

    assert!(out.contains("recorded 3 commits"), "{out}");
    assert!(
        out.contains("secs_since_prev"),
        "the warning is printed: {out}"
    );
}

#[test]
fn backfilled_commits_carry_no_gap_signal() {
    let (repo, _) = repo_with_history();
    init_madelog(&repo);
    madelog::run_capture(repo.root(), &["backfill", "--yes"]).unwrap();

    for commit in commit_payloads(&repo) {
        assert_eq!(
            commit.secs_since_prev, None,
            "they were all recorded in the same second; a gap would be a lie"
        );
    }
}

#[test]
fn backfill_skips_commits_the_hook_already_recorded() {
    let (repo, shas) = repo_with_history();
    init_madelog(&repo);
    // The hook picks up HEAD, as it would after `madelog init` and a commit.
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();
    assert_eq!(commit_payloads(&repo).len(), 1);

    let out = madelog::run_capture(repo.root(), &["backfill", "--yes"]).unwrap();

    let commits = commit_payloads(&repo);
    assert_eq!(commits.len(), 3, "the other two, and no duplicate: {out}");
    let mut recorded: Vec<&str> = commits.iter().map(|c| c.sha.as_str()).collect();
    recorded.sort_unstable();
    let mut expected: Vec<&str> = shas.iter().map(String::as_str).collect();
    expected.sort_unstable();
    assert_eq!(recorded, expected);
}

#[test]
fn backfill_twice_changes_nothing_the_second_time() {
    let (repo, _) = repo_with_history();
    init_madelog(&repo);
    madelog::run_capture(repo.root(), &["backfill", "--yes"]).unwrap();
    let before = events(&repo).len();

    let out = madelog::run_capture(repo.root(), &["backfill", "--yes"]).unwrap();

    assert_eq!(events(&repo).len(), before);
    assert!(out.contains("already in the log"), "{out}");
}

#[test]
fn backfill_since_a_sha_starts_at_that_commit() {
    let (repo, shas) = repo_with_history();
    init_madelog(&repo);

    madelog::run_capture(
        repo.root(),
        &["backfill", "--since", &shas[1][..7], "--yes"],
    )
    .unwrap();

    let recorded: Vec<String> = commit_payloads(&repo).into_iter().map(|c| c.sha).collect();
    assert_eq!(recorded, vec![shas[1].clone(), shas[2].clone()]);
}

#[test]
fn backfill_since_a_date_starts_at_that_day() {
    let repo = init_repo();
    repo.write("old.rs", "// old\n");
    let old = repo.commit_all_at("feat: january", 1_767_258_000); // 2026-01-01
    repo.write("new.rs", "// new\n");
    let new = repo.commit_all_at("feat: march", MARCH);
    init_madelog(&repo);

    madelog::run_capture(repo.root(), &["backfill", "--since", "2026-02-01", "--yes"]).unwrap();

    let recorded: Vec<String> = commit_payloads(&repo).into_iter().map(|c| c.sha).collect();
    assert_eq!(
        recorded,
        vec![new],
        "the January commit is older than --since"
    );
    assert!(!recorded.contains(&old));
}

#[test]
fn backfill_since_nonsense_is_refused() {
    let (repo, _) = repo_with_history();
    init_madelog(&repo);
    let err =
        madelog::run_in(repo.root(), &["backfill", "--since", "yesterday", "--yes"]).unwrap_err();
    assert!(format!("{err:#}").contains("2026-01-31"), "{err:#}");
}

#[test]
fn backfill_refuses_without_a_madelog_directory() {
    let (repo, _) = repo_with_history();
    isolate_device_key();
    let err = madelog::run_in(repo.root(), &["backfill", "--yes"]).unwrap_err();
    assert!(format!("{err:#}").contains("madelog init"), "{err:#}");
    assert!(!repo.path(".madelog/log.db").exists());
}

#[test]
fn backfill_without_yes_asks_first_and_an_empty_answer_means_yes() {
    let (repo, _) = repo_with_history();
    init_madelog(&repo);

    let mut out: Vec<u8> = Vec::new();
    madelog::run_in_with(repo.root(), &["backfill"], std::io::empty(), &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();

    assert!(out.contains("Record them now?"), "{out}");
    assert_eq!(commit_payloads(&repo).len(), 3, "{out}");
}

#[test]
fn backfill_without_yes_stops_when_the_answer_is_no() {
    let (repo, _) = repo_with_history();
    init_madelog(&repo);

    let mut out: Vec<u8> = Vec::new();
    madelog::run_in_with(repo.root(), &["backfill"], &b"n\n"[..], &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();

    assert!(out.contains("Nothing changed"), "{out}");
    assert!(commit_payloads(&repo).is_empty(), "{out}");
}

#[test]
fn backfill_in_a_repository_without_commits_says_so() {
    let repo = init_repo();
    init_madelog(&repo);
    let out = madelog::run_capture(repo.root(), &["backfill", "--yes"]).unwrap();
    assert!(out.contains("no commits"), "{out}");
}

#[test]
fn a_backfilled_page_counts_the_days_the_work_took() {
    let repo = init_repo();
    // Four commits, one a day, all long before madelog arrived.
    for (i, message) in ["a", "b", "c", "d"].iter().enumerate() {
        repo.write(&format!("f{i}.rs"), "// x\n");
        repo.commit_all_at(message, MARCH + 86_400 * i as i64);
    }
    init_madelog(&repo);
    madelog::run_capture(repo.root(), &["backfill", "--yes"]).unwrap();

    let ctx = madelog::page::build_context(repo.root(), None).unwrap();
    assert_eq!(ctx.stats.commits, 4);
    assert_eq!(
        ctx.stats.days_active, 4,
        "four calendar days of work, not one backfill"
    );
    // The header opens on the day the work started, not the day madelog was
    // installed. (It closes today, because `madelog init` recorded an intent
    // today, and that is part of the record too.)
    assert!(
        ctx.project
            .first_ts
            .as_deref()
            .unwrap()
            .starts_with("2026-03-02"),
        "{:?}",
        ctx.project.first_ts
    );
    // The badge still certifies the chain, which was written today.
    assert!(
        !ctx.badge
            .first_ts
            .as_deref()
            .unwrap()
            .starts_with("2026-03"),
        "the badge is chain-based: {:?}",
        ctx.badge.first_ts
    );

    // The timeline reads in the order the work happened, with the commits'
    // own dates on it.
    let commits: Vec<&madelog::page::TimelineEntry> =
        ctx.timeline.iter().filter(|e| e.kind == "commit").collect();
    assert_eq!(commits.len(), 4);
    assert!(
        commits[0].ts.starts_with("2026-03-02"),
        "{:?}",
        commits[0].ts
    );
    assert!(
        commits[3].ts.starts_with("2026-03-05"),
        "{:?}",
        commits[3].ts
    );
    assert!(commits[0].x < commits[3].x, "the axis runs forwards");
}
