//! `madelog doctor` — what is broken, and what `--fix` can put right.
//!
//! The server and Ollama checks reach for the network. They are written so
//! that both answers are fine: what matters is that they never turn an
//! offline machine into a failing one.

mod common;

use common::*;

/// A repo that is set up, with no server to talk to.
fn healthy_repo() -> TestRepo {
    let repo = init_repo();
    madelog_on_path();
    init_madelog(&repo);
    // Nothing is listening on this port, so the server check warns rather
    // than waiting thirty seconds for the real one.
    madelog::run_in(
        repo.root(),
        &["config", "set", "server.url", "http://127.0.0.1:9"],
    )
    .unwrap();
    madelog::run_in(repo.root(), &["config", "set", "llm.provider", "none"]).unwrap();
    repo.write("a.rs", "// a\n");
    repo.commit_all("first");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();
    repo
}

#[test]
fn doctor_passes_a_repository_that_is_set_up() {
    let repo = healthy_repo();
    let out = madelog::run_capture(repo.root(), &["doctor"]).unwrap();

    assert!(out.contains("ok    log"), "{out}");
    assert!(out.contains("chain verifies"), "{out}");
    assert!(out.contains("ok    post-commit"), "{out}");
    assert!(out.contains("ok    post-merge"), "{out}");
    assert!(out.contains("ok    post-rewrite"), "{out}");
    assert!(out.contains("ok    git exclude"), "{out}");
    assert!(out.contains("ok    device key"), "{out}");
    assert!(out.contains("ollama"), "{out}");
    assert!(out.contains("server"), "{out}");
    assert!(
        !out.contains("FAIL"),
        "a working repository has nothing to fail: {out}"
    );
}

#[test]
fn doctor_exits_one_when_something_is_actually_broken() {
    let repo = healthy_repo();
    std::fs::remove_file(repo.path(".git/hooks/post-commit")).unwrap();

    let err = madelog::run_in(repo.root(), &["doctor"]).unwrap_err();
    assert_eq!(exit_code(&err), Some(1), "{err:#}");
}

#[test]
fn doctor_reports_a_missing_hook_and_fix_reinstalls_it() {
    let repo = healthy_repo();
    std::fs::remove_file(repo.path(".git/hooks/post-merge")).unwrap();

    let out = madelog::run_capture(repo.root(), &["doctor"]).unwrap_err();
    let out = format!("{out:#}");
    assert!(out.contains("check"), "{out}");

    let fixed = madelog::run_capture(repo.root(), &["doctor", "--fix"]).unwrap();
    assert!(fixed.contains("ok    post-merge"), "{fixed}");
    assert!(fixed.contains("reinstalled"), "{fixed}");

    let hook = std::fs::read_to_string(repo.path(".git/hooks/post-merge")).unwrap();
    assert!(hook.contains("hook post-merge"), "{hook}");

    // And the second run has nothing left to say about it.
    let again = madelog::run_capture(repo.root(), &["doctor"]).unwrap();
    assert!(again.contains("ok    post-merge"), "{again}");
}

#[test]
fn doctor_repoints_a_hook_at_a_binary_that_no_longer_exists() {
    let repo = healthy_repo();
    std::fs::write(
        repo.path(".git/hooks/post-commit"),
        "#!/bin/sh\necho mine\n# madelog-cli:post-commit - old\n/gone/madelog hook post-commit || true\n",
    )
    .unwrap();

    let err = madelog::run_in(repo.root(), &["doctor"]).unwrap_err();
    assert_eq!(exit_code(&err), Some(1), "{err:#}");

    let fixed = madelog::run_capture(repo.root(), &["doctor", "--fix"]).unwrap();
    assert!(fixed.contains("ok    post-commit"), "{fixed}");

    let hook = std::fs::read_to_string(repo.path(".git/hooks/post-commit")).unwrap();
    assert!(
        hook.contains("echo mine"),
        "the user's own lines stay: {hook}"
    );
    assert!(!hook.contains("/gone/madelog"), "{hook}");
}

#[test]
fn doctor_puts_madelog_back_into_the_git_exclude() {
    let repo = healthy_repo();
    std::fs::write(repo.path(".git/info/exclude"), "# nothing here\n").unwrap();

    let err = madelog::run_in(repo.root(), &["doctor"]).unwrap_err();
    assert_eq!(exit_code(&err), Some(1), "{err:#}");

    let fixed = madelog::run_capture(repo.root(), &["doctor", "--fix"]).unwrap();
    assert!(fixed.contains("ok    git exclude"), "{fixed}");
    let body = std::fs::read_to_string(repo.path(".git/info/exclude")).unwrap();
    assert!(body.contains(".madelog/"), "{body}");
    assert!(body.contains("# nothing here"), "{body}");
}

#[test]
fn doctor_fails_when_the_log_is_missing() {
    let repo = init_repo();
    isolate_device_key();
    madelog_on_path();

    let err = madelog::run_in(repo.root(), &["doctor"]).unwrap_err();
    assert_eq!(exit_code(&err), Some(1), "{err:#}");
}

#[test]
fn doctor_fails_when_the_chain_is_broken() {
    let repo = healthy_repo();
    // Rewrite one payload behind the log's back. The triggers refuse an
    // UPDATE, so this is the only honest way to forge one.
    let db = repo.path(".madelog/log.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("DROP TRIGGER events_no_update;")
        .unwrap();
    conn.execute(
        "UPDATE events SET payload = '{\"private\":false,\"text\":\"forged\"}' WHERE seq = 1",
        [],
    )
    .unwrap();
    drop(conn);

    let err = madelog::run_in(repo.root(), &["doctor"]).unwrap_err();
    assert_eq!(exit_code(&err), Some(1), "{err:#}");
}

#[test]
fn doctor_warns_rather_than_fails_when_the_server_is_not_answering() {
    let repo = healthy_repo();
    let out = madelog::run_capture(repo.root(), &["doctor"]).unwrap();
    assert!(out.contains("warn  server"), "{out}");
    assert!(out.contains("did not answer"), "{out}");
    assert!(
        out.contains("nothing is broken"),
        "an offline machine is not a broken one: {out}"
    );
}

#[test]
fn doctor_reports_on_every_ai_tool_and_on_the_recording_itself() {
    let _lock = capture_lock();
    let repo = init_repo();
    init_madelog(&repo);
    let _home = fake_claude(&repo, chrono::Utc::now());

    let out = madelog::run_capture(repo.root(), &["doctor"]).unwrap_or_else(|e| format!("{e:#}"));

    for tool in ["claude-code", "codex", "cursor", "copilot"] {
        assert!(
            out.lines().any(|l| l.contains(tool)),
            "no line for {tool}:\n{out}"
        );
    }
    let claude = out.lines().find(|l| l.contains("claude-code")).unwrap();
    assert!(claude.trim_start().starts_with("ok"), "{claude}");
    assert!(claude.contains("1 session touches this repo"), "{claude}");

    let sessions = out
        .lines()
        .find(|l| l.contains("ai sessions"))
        .unwrap_or_else(|| panic!("no ai sessions line:\n{out}"));
    assert!(sessions.contains("recording"), "{sessions}");
    assert!(sessions.trim_start().starts_with("ok"), "{sessions}");
}

fn add_session(repo: &TestRepo, sha: &str) {
    use madelog::event::{AiSessionPayload, Payload};
    let store = madelog::store::Store::open(&repo.path(".madelog/log.db")).unwrap();
    store
        .append_payload(
            Some(sha),
            &Payload::AiSession(AiSessionPayload {
                tool: "claude-code".into(),
                model: None,
                session_id: "sess-1".into(),
                prompt: None,
                response_excerpt: None,
                files: vec![],
                accepted: true,
            }),
        )
        .unwrap();
}

#[test]
fn doctor_coverage_says_so_when_no_sessions_are_recorded() {
    let repo = healthy_repo();
    let out = madelog::run_capture(repo.root(), &["doctor"]).unwrap();
    assert!(
        out.contains("ok    coverage") && out.contains("no AI sessions recorded"),
        "{out}"
    );
}

#[test]
fn doctor_coverage_warns_when_few_commits_carry_a_session() {
    let repo = healthy_repo();
    for i in 0..3 {
        repo.write("b.rs", &format!("// b{i}\n"));
        repo.commit_all(&format!("more {i}"));
        madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();
    }
    let sha = repo.commit_all("anchor");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();
    add_session(&repo, &sha);

    let out = madelog::run_capture(repo.root(), &["doctor"]).unwrap();
    assert!(out.contains("warn  coverage"), "{out}");
    assert!(out.contains("only 1 of 5 commits"), "{out}");
    assert!(!out.contains("FAIL"), "{out}");
}
