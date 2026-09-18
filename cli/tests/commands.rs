mod common;

use common::*;
use madelog::event::{EventKind, HidePayload};

#[test]
fn config_get_and_set_round_trip() {
    let repo = init_repo();
    init_madelog(&repo);

    assert_eq!(
        madelog::run_capture(repo.root(), &["config", "get", "llm.provider"])
            .unwrap()
            .trim(),
        "auto"
    );

    madelog::run_in(repo.root(), &["config", "set", "llm.model", "qwen3:4b"]).unwrap();
    madelog::run_in(repo.root(), &["config", "set", "llm.remote", "true"]).unwrap();
    madelog::run_in(
        repo.root(),
        &["config", "set", "server.url", "https://example.test"],
    )
    .unwrap();
    madelog::run_in(
        repo.root(),
        &["config", "set", "privacy.redact", "^secret/, \\.env$"],
    )
    .unwrap();
    madelog::run_in(
        repo.root(),
        &["config", "set", "project.delivered_url", "https://a.test"],
    )
    .unwrap();

    assert_eq!(
        madelog::run_capture(repo.root(), &["config", "get", "llm.model"])
            .unwrap()
            .trim(),
        "qwen3:4b"
    );
    assert_eq!(
        madelog::run_capture(repo.root(), &["config", "get", "llm.remote"])
            .unwrap()
            .trim(),
        "true"
    );
    assert_eq!(
        madelog::run_capture(repo.root(), &["config", "get", "server.url"])
            .unwrap()
            .trim(),
        "https://example.test"
    );
    assert_eq!(
        madelog::run_capture(repo.root(), &["config", "get", "privacy.redact"])
            .unwrap()
            .trim(),
        "^secret/\n\\.env$"
    );
    assert_eq!(
        madelog::run_capture(repo.root(), &["config", "get", "project.delivered_url"])
            .unwrap()
            .trim(),
        "https://a.test"
    );

    // Values survive a reload from disk.
    let cfg = madelog::config::Config::load(&repo.path(".madelog/config.toml")).unwrap();
    assert_eq!(cfg.privacy.redact, vec!["^secret/", "\\.env$"]);
    assert!(cfg.llm.remote);
}

#[test]
fn config_rejects_unknown_keys() {
    let repo = init_repo();
    init_madelog(&repo);
    let err = madelog::run_in(repo.root(), &["config", "set", "llm.temperature", "7"]).unwrap_err();
    assert!(format!("{err:#}").contains("unknown config key"), "{err:#}");
}

#[test]
fn hide_only_accepts_ai_session_events() {
    let repo = init_repo();
    init_madelog(&repo);
    madelog::run_in(repo.root(), &["note", "a decision"]).unwrap();

    let decision_id = events(&repo).last().unwrap().id.clone();
    let err = madelog::run_in(repo.root(), &["hide", &decision_id]).unwrap_err();
    assert!(format!("{err:#}").contains("ai_session"), "{err:#}");

    let err = madelog::run_in(repo.root(), &["hide", "not-an-id"]).unwrap_err();
    assert!(format!("{err:#}").contains("no event"), "{err:#}");
}

#[test]
fn hide_appends_a_hide_event_for_an_ai_session() {
    let repo = init_repo();
    init_madelog(&repo);

    // Seed an ai_session directly; the Git AI reader arrives in a later task.
    let store = madelog::store::Store::open(&repo.path(".madelog/log.db")).unwrap();
    let session = store
        .append_payload(
            None,
            &madelog::event::Payload::AiSession(madelog::event::AiSessionPayload {
                tool: "claude-code".into(),
                model: Some("opus".into()),
                session_id: "s-1".into(),
                prompt: Some("write the parser".into()),
                response_excerpt: None,
                files: vec![],
                accepted: true,
            }),
        )
        .unwrap();
    drop(store);

    madelog::run_in(repo.root(), &["hide", &session.id]).unwrap();

    let last = events(&repo).pop().unwrap();
    assert_eq!(last.kind, EventKind::Hide);
    let h: HidePayload = serde_json::from_value(last.payload.clone()).unwrap();
    assert_eq!(h.target_id, session.id);
    assert_eq!(h.target_kind, "ai_session");

    // The hidden session stays in the chain.
    assert!(events(&repo).iter().any(|e| e.id == session.id));
    madelog::run_capture(repo.root(), &["verify"]).expect("chain still verifies");
}

#[test]
fn deferred_commands_exit_with_code_two() {
    let repo = init_repo();
    init_madelog(&repo);

    for (args, needle) in [
        (vec!["model", "pull"], "not available in this version"),
        (
            vec!["ask", "what", "happened"],
            "not available in this version",
        ),
    ] {
        let err = madelog::run_in(repo.root(), &args).unwrap_err();
        assert_eq!(exit_code(&err), Some(2), "{args:?}: {err:#}");
        assert!(format!("{err:#}").contains(needle), "{args:?}: {err:#}");
    }
}
