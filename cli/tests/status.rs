//! `madelog status` — one screen, and the same facts as JSON.

mod common;

use common::*;

/// 2026-03-02T09:00:00Z.
const MARCH: i64 = 1_772_442_000;

fn status_json(repo: &TestRepo) -> serde_json::Value {
    let out = madelog::run_capture(repo.root(), &["status", "--json"]).unwrap();
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("status --json is JSON ({e}): {out}"))
}

#[test]
fn status_names_the_repo_the_log_and_the_last_commit() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.rs", "// a\n");
    repo.commit_all_at("feat: a thing", MARCH);
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();
    madelog::run_in(repo.root(), &["note", "chose sqlite"]).unwrap();

    let out = madelog::run_capture(repo.root(), &["status"]).unwrap();

    assert!(out.contains("repo"), "{out}");
    assert!(out.contains("commit 1"), "events by kind: {out}");
    assert!(out.contains("decision 1"), "{out}");
    assert!(out.contains("intent 1"), "{out}");
    assert!(out.contains("feat: a thing"), "the last commit: {out}");
    assert!(
        out.contains("committed 2026-03-02T09:00:00Z"),
        "the real commit time: {out}"
    );
    assert!(
        out.contains("recorded 20"),
        "and when madelog saw it: {out}"
    );
}

#[test]
fn status_counts_commits_git_has_that_the_log_does_not() {
    let repo = init_repo();
    for (i, m) in ["one", "two", "three"].iter().enumerate() {
        repo.write(&format!("f{i}.rs"), "// x\n");
        repo.commit_all_at(m, MARCH + 3600 * i as i64);
    }
    init_madelog_without_backfill(&repo);

    let out = madelog::run_capture(repo.root(), &["status"]).unwrap();
    assert!(out.contains("3 commits in git are not recorded"), "{out}");
    assert!(out.contains("madelog backfill"), "the hint: {out}");
    assert_eq!(status_json(&repo)["unrecorded_commits"], 3);

    madelog::run_capture(repo.root(), &["backfill", "--yes"]).unwrap();

    let out = madelog::run_capture(repo.root(), &["status"]).unwrap();
    assert!(
        out.contains("every commit on this branch is recorded"),
        "{out}"
    );
    assert_eq!(status_json(&repo)["unrecorded_commits"], 0);
}

#[test]
fn status_names_every_hook_and_the_binary_it_calls() {
    let repo = init_repo();
    init_madelog(&repo);

    let out = madelog::run_capture(repo.root(), &["status"]).unwrap();
    for hook in ["post-commit", "post-merge", "post-rewrite"] {
        assert!(out.contains(hook), "{hook} is missing from: {out}");
    }
    assert!(out.contains("calls madelog"), "{out}");

    let json = status_json(&repo);
    let hooks = json["hooks"].as_array().unwrap();
    assert_eq!(hooks.len(), 3);
    for hook in hooks {
        assert_eq!(hook["installed"], true, "{hook}");
        assert_eq!(hook["command"], "madelog", "{hook}");
    }
}

#[test]
fn status_notices_a_hook_that_was_removed() {
    let repo = init_repo();
    init_madelog(&repo);
    std::fs::remove_file(repo.path(".git/hooks/post-merge")).unwrap();

    let out = madelog::run_capture(repo.root(), &["status"]).unwrap();
    assert!(out.contains("post-merge    not installed"), "{out}");
    assert!(out.contains("madelog doctor --fix"), "{out}");
}

#[test]
fn status_reports_the_server_and_the_page() {
    let repo = init_repo();
    init_madelog(&repo);

    let out = madelog::run_capture(repo.root(), &["status"]).unwrap();
    assert!(out.contains("not published"), "{out}");
    assert!(out.contains("not rendered yet"), "{out}");

    let json = status_json(&repo);
    assert!(json["server_url"].as_str().unwrap().starts_with("http"));
    assert_eq!(json["published"], serde_json::Value::Null);
    assert_eq!(json["page"]["rendered"], false);
    assert_eq!(json["page"]["newer_than_publish"], false);
}

#[test]
fn status_says_when_the_page_on_disk_is_newer_than_the_one_online() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.rs", "// a\n");
    repo.commit_all("first");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    // A page published long ago, and rendered just now.
    madelog::run_in(
        repo.root(),
        &["config", "set", "server.slug", "quiet-fern-4821"],
    )
    .unwrap();
    madelog::run_in(
        repo.root(),
        &[
            "config",
            "set",
            "server.published_at",
            "2020-01-01T00:00:00Z",
        ],
    )
    .unwrap();
    madelog::run_capture(repo.root(), &["publish", "--local", "--yes"]).unwrap();

    let out = madelog::run_capture(repo.root(), &["status"]).unwrap();
    assert!(out.contains("quiet-fern-4821"), "{out}");
    assert!(out.contains("/p/quiet-fern-4821"), "the url: {out}");
    assert!(
        out.contains("the page on disk is newer"),
        "the nudge to republish: {out}"
    );
    assert_eq!(status_json(&repo)["page"]["newer_than_publish"], true);
}

#[test]
fn status_is_quiet_when_the_publish_is_newer_than_the_page() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.rs", "// a\n");
    repo.commit_all("first");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();
    madelog::run_capture(repo.root(), &["publish", "--local", "--yes"]).unwrap();
    madelog::run_in(
        repo.root(),
        &["config", "set", "server.slug", "quiet-fern-4821"],
    )
    .unwrap();
    madelog::run_in(
        repo.root(),
        &[
            "config",
            "set",
            "server.published_at",
            "2999-01-01T00:00:00Z",
        ],
    )
    .unwrap();

    let out = madelog::run_capture(repo.root(), &["status"]).unwrap();
    assert!(!out.contains("the page on disk is newer"), "{out}");
    assert_eq!(status_json(&repo)["page"]["newer_than_publish"], false);
}

#[test]
fn status_json_carries_git_ai_and_ollama() {
    let repo = init_repo();
    init_madelog(&repo);
    madelog::run_in(repo.root(), &["config", "set", "llm.provider", "none"]).unwrap();

    let json = status_json(&repo);
    assert_eq!(json["ollama"]["provider"], "none");
    assert_eq!(json["ollama"]["reachable"], false);
    assert_eq!(json["ollama"]["model"], "qwen3:4b");
    assert!(json["git_ai"].is_string() || json["git_ai"].is_null());

    let out = madelog::run_capture(repo.root(), &["status"]).unwrap();
    assert!(out.contains("off (llm.provider = none)"), "{out}");
}

#[test]
fn status_in_a_repository_without_madelog_says_so_rather_than_failing() {
    let repo = init_repo();
    isolate_device_key();
    let out = madelog::run_capture(repo.root(), &["status"]).unwrap();
    assert!(out.contains("nothing recorded yet"), "{out}");
    assert!(out.contains("madelog init"), "{out}");
}

#[test]
fn status_lists_every_ai_tool_and_whether_sessions_are_recorded() {
    let _lock = capture_lock();
    let repo = init_repo();
    init_madelog(&repo);
    let _home = fake_claude(&repo, chrono::Utc::now());

    let out = madelog::run_capture(repo.root(), &["status"]).unwrap();

    assert!(out.contains("ai tools"), "{out}");
    for tool in ["claude-code", "codex", "cursor", "copilot"] {
        assert!(out.contains(tool), "no line for {tool}:\n{out}");
    }
    assert!(out.contains("1 session touches this repo"), "{out}");
    assert!(
        out.lines()
            .any(|l| l.starts_with("ai sessions") && l.contains("recording")),
        "{out}"
    );

    let json = status_json(&repo);
    let tools = json["ai_tools"].as_array().unwrap();
    assert_eq!(tools.len(), 4);
    let claude = tools
        .iter()
        .find(|t| t["name"] == "claude-code")
        .expect("claude-code in the json");
    assert_eq!(claude["installed"], true);
    assert_eq!(claude["sessions"], 1);
    assert!(json["ai_sessions"].as_str().unwrap().contains("recording"));
}

#[test]
fn status_says_why_ai_sessions_are_not_being_recorded() {
    let _lock = capture_lock();
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

    let out = madelog::run_capture(repo.root(), &["status"]).unwrap();
    let line = out
        .lines()
        .find(|l| l.starts_with("ai sessions"))
        .unwrap_or_else(|| panic!("no ai sessions line:\n{out}"));
    assert!(line.contains("not recording"), "{line}");
    assert!(line.contains("madelog init"), "and why: {line}");
}
