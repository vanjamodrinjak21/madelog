//! The hash chain is the product. These vectors are computed independently
//! (sha256 of the documented byte string) and must never drift.

use pop::event::{canonical_json, hash_for, EventKind, IntentPayload, Payload, GENESIS_PREV_HASH};

#[test]
fn genesis_prev_hash_is_sixty_four_zeros() {
    assert_eq!(GENESIS_PREV_HASH, "0".repeat(64));
}

#[test]
fn canonical_json_sorts_keys_and_drops_whitespace() {
    let v = serde_json::json!({
        "zed": 1,
        "alpha": { "b": [1, 2, {"y": true, "x": null}], "a": "x" },
        "mid": "m"
    });
    assert_eq!(
        canonical_json(&v),
        r#"{"alpha":{"a":"x","b":[1,2,{"x":null,"y":true}]},"mid":"m","zed":1}"#
    );
}

#[test]
fn intent_payload_canonicalises_as_specified() {
    let payload = Payload::Intent(IntentPayload {
        what: "a pop cli".into(),
        for_whom: "a small studio".into(),
        tools: vec!["claude-code".into()],
        record_ai_sessions: true,
    });
    assert_eq!(
        canonical_json(&payload.to_value()),
        r#"{"for_whom":"a small studio","record_ai_sessions":true,"tools":["claude-code"],"what":"a pop cli"}"#
    );
}

#[test]
fn hash_for_matches_the_published_test_vector() {
    let payload = serde_json::json!({
        "what": "a pop cli",
        "for_whom": "a small studio",
        "tools": ["claude-code"],
        "record_ai_sessions": true,
    });
    let hash = hash_for(
        GENESIS_PREV_HASH,
        "2026-09-17T09:00:00Z",
        EventKind::Intent,
        &payload,
    );
    assert_eq!(
        hash,
        "acca5e4cbe2f6322f3db2ba4418cbcda1c923a02b8904acf9c6545afce20806e"
    );
    assert_eq!(hash.len(), 64);
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
}

#[test]
fn every_event_kind_serialises_to_its_schema_name() {
    let names = [
        (EventKind::Intent, "intent"),
        (EventKind::Commit, "commit"),
        (EventKind::AiSession, "ai_session"),
        (EventKind::Decision, "decision"),
        (EventKind::Pattern, "pattern"),
        (EventKind::Summary, "summary"),
        (EventKind::Hide, "hide"),
    ];
    for (kind, name) in names {
        assert_eq!(kind.as_str(), name);
        assert_eq!(name.parse::<EventKind>().unwrap(), kind);
        assert_eq!(serde_json::to_value(kind).unwrap(), serde_json::json!(name));
    }
}

#[test]
fn a_change_anywhere_in_the_input_changes_the_hash() {
    let payload = serde_json::json!({"private": false, "text": "a"});
    let base = hash_for(GENESIS_PREV_HASH, "2026-09-17T09:00:00Z", EventKind::Decision, &payload);
    let other_ts = hash_for(GENESIS_PREV_HASH, "2026-09-17T09:00:01Z", EventKind::Decision, &payload);
    let other_kind = hash_for(GENESIS_PREV_HASH, "2026-09-17T09:00:00Z", EventKind::Summary, &payload);
    let other_prev = hash_for(&"1".repeat(64), "2026-09-17T09:00:00Z", EventKind::Decision, &payload);
    let other_payload = hash_for(
        GENESIS_PREV_HASH,
        "2026-09-17T09:00:00Z",
        EventKind::Decision,
        &serde_json::json!({"private": true, "text": "a"}),
    );
    let all = [&base, &other_ts, &other_kind, &other_prev, &other_payload];
    for (i, a) in all.iter().enumerate() {
        for b in all.iter().skip(i + 1) {
            assert_ne!(a, b);
        }
    }
}
