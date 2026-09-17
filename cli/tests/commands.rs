mod common;

use common::*;
use pop::event::{EventKind, HidePayload};

#[test]
fn config_get_and_set_round_trip() {
    let repo = init_repo();
    init_pop(&repo);

    assert_eq!(
        pop::run_capture(repo.root(), &["config", "get", "llm.provider"])
            .unwrap()
            .trim(),
        "auto"
    );

    pop::run_in(repo.root(), &["config", "set", "llm.model", "qwen3:4b"]).unwrap();
    pop::run_in(repo.root(), &["config", "set", "llm.remote", "true"]).unwrap();
    pop::run_in(
        repo.root(),
        &["config", "set", "server.url", "https://example.test"],
    )
    .unwrap();
    pop::run_in(
        repo.root(),
        &["config", "set", "privacy.redact", "^secret/, \\.env$"],
    )
    .unwrap();
    pop::run_in(repo.root(), &["config", "set", "project.delivered_url", "https://a.test"]).unwrap();

    assert_eq!(
        pop::run_capture(repo.root(), &["config", "get", "llm.model"])
            .unwrap()
            .trim(),
        "qwen3:4b"
    );
    assert_eq!(
        pop::run_capture(repo.root(), &["config", "get", "llm.remote"])
            .unwrap()
            .trim(),
        "true"
    );
    assert_eq!(
        pop::run_capture(repo.root(), &["config", "get", "server.url"])
            .unwrap()
            .trim(),
        "https://example.test"
    );
    assert_eq!(
        pop::run_capture(repo.root(), &["config", "get", "privacy.redact"])
            .unwrap()
            .trim(),
        "^secret/\n\\.env$"
    );
    assert_eq!(
        pop::run_capture(repo.root(), &["config", "get", "project.delivered_url"])
            .unwrap()
            .trim(),
        "https://a.test"
    );

    // Values survive a reload from disk.
    let cfg = pop::config::Config::load(&repo.path(".pop/config.toml")).unwrap();
    assert_eq!(cfg.privacy.redact, vec!["^secret/", "\\.env$"]);
    assert!(cfg.llm.remote);
}

#[test]
fn config_rejects_unknown_keys() {
    let repo = init_repo();
    init_pop(&repo);
    let err = pop::run_in(repo.root(), &["config", "set", "llm.temperature", "7"]).unwrap_err();
    assert!(format!("{err:#}").contains("unknown config key"), "{err:#}");
}

#[test]
fn hide_only_accepts_ai_session_events() {
    let repo = init_repo();
    init_pop(&repo);
    pop::run_in(repo.root(), &["note", "a decision"]).unwrap();

    let decision_id = events(&repo).last().unwrap().id.clone();
    let err = pop::run_in(repo.root(), &["hide", &decision_id]).unwrap_err();
    assert!(format!("{err:#}").contains("ai_session"), "{err:#}");

    let err = pop::run_in(repo.root(), &["hide", "not-an-id"]).unwrap_err();
    assert!(format!("{err:#}").contains("no event"), "{err:#}");
}

#[test]
fn hide_appends_a_hide_event_for_an_ai_session() {
    let repo = init_repo();
    init_pop(&repo);

    // Seed an ai_session directly; the Git AI reader arrives in a later task.
    let store = pop::store::Store::open(&repo.path(".pop/log.db")).unwrap();
    let session = store
        .append_payload(
            None,
            &pop::event::Payload::AiSession(pop::event::AiSessionPayload {
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

    pop::run_in(repo.root(), &["hide", &session.id]).unwrap();

    let last = events(&repo).pop().unwrap();
    assert_eq!(last.kind, EventKind::Hide);
    let h: HidePayload = serde_json::from_value(last.payload.clone()).unwrap();
    assert_eq!(h.target_id, session.id);
    assert_eq!(h.target_kind, "ai_session");

    // The hidden session stays in the chain.
    assert!(events(&repo).iter().any(|e| e.id == session.id));
    pop::run_capture(repo.root(), &["verify"]).expect("chain still verifies");
}

#[test]
fn deferred_commands_exit_with_code_two() {
    let repo = init_repo();
    init_pop(&repo);

    for (args, needle) in [
        (vec!["model", "pull"], "not available in this version"),
        (vec!["ask", "what", "happened"], "not available in this version"),
        (vec!["publish"], "not implemented yet"),
        (vec!["analyze"], "not implemented yet"),
        (vec!["summarize"], "not implemented yet"),
        (vec!["summary", "edit", "some-id"], "not implemented yet"),
    ] {
        let err = pop::run_in(repo.root(), &args).unwrap_err();
        assert_eq!(exit_code(&err), Some(2), "{args:?}: {err:#}");
        assert!(format!("{err:#}").contains(needle), "{args:?}: {err:#}");
    }
}
