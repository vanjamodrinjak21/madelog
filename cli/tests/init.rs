mod common;

use common::*;
use madelog::event::{EventKind, IntentPayload};

#[test]
fn init_creates_files_and_intent_event() {
    let repo = init_repo();
    init_madelog(&repo);

    assert!(
        repo.path(".madelog/intent.md").is_file(),
        "intent.md written"
    );
    assert!(
        repo.path(".madelog/config.toml").is_file(),
        "config.toml written"
    );
    assert!(repo.path(".madelog/log.db").is_file(), "log.db created");
    assert!(
        repo.path(".git/hooks/post-commit").is_file(),
        "post-commit hook installed"
    );

    let intent_md = std::fs::read_to_string(repo.path(".madelog/intent.md")).unwrap();
    assert!(intent_md.contains("a small cli that records process"));
    assert!(intent_md.contains("a studio hiring juniors"));

    let exclude = std::fs::read_to_string(repo.path(".git/info/exclude")).unwrap();
    assert!(
        exclude.lines().any(|l| l.trim() == ".madelog/"),
        "{exclude}"
    );

    let hook = std::fs::read_to_string(repo.path(".git/hooks/post-commit")).unwrap();
    assert!(hook.contains("madelog hook post-commit"), "{hook}");

    let evs = events(&repo);
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].kind, EventKind::Intent);
    assert_eq!(evs[0].seq, 1);
    assert_eq!(evs[0].prev_hash, madelog::event::GENESIS_PREV_HASH);

    let intent: IntentPayload = serde_json::from_value(evs[0].payload.clone()).unwrap();
    assert_eq!(intent.what, "a small cli that records process");
    assert_eq!(intent.for_whom, "a studio hiring juniors");
    assert_eq!(intent.tools, vec!["claude-code", "cursor"]);
    assert!(intent.record_ai_sessions);
}

#[test]
fn init_answers_prompts_from_stdin_when_flags_are_absent() {
    let repo = init_repo();
    isolate_device_key();
    let answers = "a tiny parser\nmy future employer\ncursor,codex\nn\n";
    madelog::run_in_with(repo.root(), &["init"], answers.as_bytes(), &mut Vec::new()).unwrap();

    let evs = events(&repo);
    let intent: IntentPayload = serde_json::from_value(evs[0].payload.clone()).unwrap();
    assert_eq!(intent.what, "a tiny parser");
    assert_eq!(intent.for_whom, "my future employer");
    assert_eq!(intent.tools, vec!["cursor", "codex"]);
    assert!(!intent.record_ai_sessions);
}

#[test]
fn init_keeps_an_existing_hook_and_is_idempotent() {
    let repo = init_repo();
    std::fs::create_dir_all(repo.path(".git/hooks")).unwrap();
    repo.write(".git/hooks/post-commit", "#!/bin/sh\necho mine\n");

    init_madelog(&repo);
    init_madelog(&repo);

    let hook = std::fs::read_to_string(repo.path(".git/hooks/post-commit")).unwrap();
    assert!(hook.contains("echo mine"), "existing hook kept: {hook}");
    assert_eq!(
        hook.matches("madelog hook post-commit").count(),
        1,
        "our line appended exactly once: {hook}"
    );

    let exclude = std::fs::read_to_string(repo.path(".git/info/exclude")).unwrap();
    assert_eq!(exclude.matches(".madelog/").count(), 1, "{exclude}");
}

#[test]
fn a_second_init_without_answers_records_nothing() {
    let repo = init_repo();
    init_madelog(&repo);
    let before = events(&repo);
    let md_before = std::fs::read_to_string(repo.path(".madelog/intent.md")).unwrap();

    let out = madelog::run_capture(repo.root(), &["init", "--yes"]).unwrap();

    assert!(out.contains("already recording"), "{out}");
    assert_eq!(events(&repo).len(), before.len(), "no second intent event");
    assert_eq!(
        std::fs::read_to_string(repo.path(".madelog/intent.md")).unwrap(),
        md_before,
        "intent.md is left alone"
    );
    // The idempotent parts still ran.
    assert!(repo.path(".git/hooks/post-commit").is_file());
}

#[test]
fn a_second_init_with_one_answer_merges_the_rest() {
    let repo = init_repo();
    init_madelog(&repo);

    madelog::run_capture(
        repo.root(),
        &["init", "--what", "a bank statement reconciler", "--yes"],
    )
    .unwrap();

    let evs = events(&repo);
    assert_eq!(evs.len(), 2, "a new intent, on top of the old one");
    assert_eq!(evs[0].kind, EventKind::Intent);
    assert_eq!(evs[1].kind, EventKind::Intent);

    let intent: IntentPayload = serde_json::from_value(evs[1].payload.clone()).unwrap();
    assert_eq!(intent.what, "a bank statement reconciler", "the flag wins");
    assert_eq!(
        intent.for_whom, "a studio hiring juniors",
        "the missing answer carries over"
    );
    assert_eq!(intent.tools, vec!["claude-code", "cursor"]);
    assert!(
        intent.record_ai_sessions,
        "and so does the AI-session answer"
    );

    let md = std::fs::read_to_string(repo.path(".madelog/intent.md")).unwrap();
    assert!(md.contains("a bank statement reconciler"), "{md}");
    assert!(md.contains("a studio hiring juniors"), "{md}");

    // `madelog export` reports the newest intent, which is what the page shows.
    let out = madelog::run_capture(repo.root(), &["export"]).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["intent"]["what"], "a bank statement reconciler");
    assert_eq!(v["intent"]["for_whom"], "a studio hiring juniors");
}

#[test]
fn init_outside_a_git_repository_fails() {
    isolate_device_key();
    let dir = tempfile::TempDir::new().unwrap();
    let err = madelog::run_in(dir.path(), &["init", "--yes"]).unwrap_err();
    assert!(
        format!("{err:#}").contains("git repository"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn init_creates_a_device_key_with_owner_only_permissions() {
    let repo = init_repo();
    init_madelog(&repo);

    let path = madelog::keys::key_path().unwrap();
    assert!(path.is_file(), "device key at {}", path.display());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "device key mode {:o}", mode & 0o777);
    }

    let key = madelog::keys::DeviceKey::load_or_create().unwrap();
    assert_eq!(key.public_key_hex().len(), 64);
    assert_eq!(key.sign(b"hello").len(), 64);
}

#[test]
fn init_warns_about_a_vague_headline_but_records_it() {
    let repo = init_repo();
    isolate_device_key();
    let out = madelog::run_capture(
        repo.root(),
        &["init", "--what", "pwa app", "--for-whom", "x", "--yes"],
    )
    .unwrap();
    assert!(
        out.contains("note: \"pwa app\" will be the headline recruiters read first"),
        "{out}"
    );
    let intent: IntentPayload = serde_json::from_value(events(&repo)[0].payload.clone()).unwrap();
    assert_eq!(intent.what, "pwa app");
}

#[test]
fn init_is_quiet_about_a_good_headline() {
    let repo = init_repo();
    isolate_device_key();
    let out = madelog::run_capture(
        repo.root(),
        &[
            "init",
            "--what",
            "A guest photo wall for live events, built as a PWA",
            "--yes",
        ],
    )
    .unwrap();
    assert!(!out.contains("note:"), "{out}");
}
