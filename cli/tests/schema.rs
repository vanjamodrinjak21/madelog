//! `schema/event.schema.json` is the single source of truth for the event
//! model, shared with the Go server. The Rust structs are hand-written, so
//! these tests hold them against it.

use std::collections::BTreeSet;

use madelog::event::*;

fn schema() -> serde_json::Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("schema")
        .join("event.schema.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&raw).expect("the schema is valid JSON")
}

fn names(value: &serde_json::Value) -> BTreeSet<String> {
    value
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect()
}

fn strings(value: &serde_json::Value) -> BTreeSet<String> {
    value
        .as_array()
        .expect("an array")
        .iter()
        .map(|v| v.as_str().expect("a string").to_string())
        .collect()
}

/// Sample payloads with every optional field populated.
fn samples() -> Vec<Payload> {
    vec![
        Payload::Intent(IntentPayload {
            what: "a cli".into(),
            for_whom: "a studio".into(),
            tools: vec!["claude-code".into()],
            record_ai_sessions: true,
        }),
        Payload::Commit(CommitPayload {
            sha: "a".repeat(40),
            message: "feat: something".into(),
            author: "A <a@example.com>".into(),
            parents: vec!["b".repeat(40)],
            files: vec![CommitFile {
                path: "src/main.rs".into(),
                added: 3,
                removed: 1,
                status: FileStatus::Modified,
                diff_excerpt: Some("@@ -1 +1 @@".into()),
            }],
            secs_since_prev: Some(90),
            committed_at: Some("2026-09-17T10:00:00Z".into()),
        }),
        Payload::AiSession(AiSessionPayload {
            tool: "claude-code".into(),
            model: Some("opus".into()),
            session_id: "s-1".into(),
            prompt: Some("write the parser".into()),
            response_excerpt: Some("here is the parser".into()),
            files: vec![AiSessionFile {
                path: "src/parse.rs".into(),
                ai_lines: 40,
                ai_lines_modified_by_human: 12,
            }],
            accepted: true,
        }),
        Payload::Decision(DecisionPayload {
            text: "chose sqlite".into(),
            private: false,
        }),
        Payload::Pattern(PatternPayload {
            pattern: PatternKind::FixAfterAi,
            commits: vec!["c".repeat(40)],
            detail: "fixed the generated parser".into(),
        }),
        Payload::Summary(SummaryPayload {
            week_start: "2026-09-01".into(),
            week_end: "2026-09-07".into(),
            text: "you shipped the chain".into(),
            generated_by: "heuristic".into(),
            edited: false,
        }),
        Payload::Hide(HidePayload {
            target_kind: "ai_session".into(),
            target_id: "01a0b11f-013b-7519-9e6f-d32722ad0007".into(),
        }),
    ]
}

#[test]
fn the_event_kinds_match_the_schema() {
    let schema = schema();
    let expected = strings(&schema["properties"]["kind"]["enum"]);
    let ours: BTreeSet<String> = samples()
        .iter()
        .map(|p| p.kind().as_str().to_string())
        .collect();
    assert_eq!(ours, expected);
}

#[test]
fn an_event_carries_exactly_the_schema_properties() {
    let schema = schema();
    let allowed = names(&schema["properties"]);
    let required = strings(&schema["required"]);
    assert_eq!(schema["additionalProperties"], serde_json::json!(false));

    let dir = tempfile::TempDir::new().unwrap();
    let store = madelog::store::Store::open(&dir.path().join("log.db")).unwrap();
    for payload in samples() {
        let event = store
            .append_payload(Some(&"a".repeat(40)), &payload)
            .unwrap();
        let value = serde_json::to_value(&event).unwrap();
        let keys = names(&value);
        assert!(
            keys.is_subset(&allowed),
            "{:?} are not in the schema",
            keys.difference(&allowed).collect::<Vec<_>>()
        );
        assert!(
            required.is_subset(&keys),
            "missing required {:?}",
            required.difference(&keys).collect::<Vec<_>>()
        );
        assert_eq!(value["hash"].as_str().unwrap().len(), 64);
        assert_eq!(value["prev_hash"].as_str().unwrap().len(), 64);
    }
}

#[test]
fn every_payload_matches_its_schema_definition() {
    let schema = schema();
    for payload in samples() {
        let kind = payload.kind();
        let def = &schema["$defs"][kind.as_str()];
        assert!(!def.is_null(), "no $defs entry for {kind}");
        assert_eq!(
            def["additionalProperties"],
            serde_json::json!(false),
            "{kind}"
        );

        let allowed = names(&def["properties"]);
        let required = strings(&def["required"]);
        let value = payload.to_value();
        let keys = names(&value);

        assert!(
            keys.is_subset(&allowed),
            "{kind}: {:?} are not in the schema",
            keys.difference(&allowed).collect::<Vec<_>>()
        );
        assert!(
            required.is_subset(&keys),
            "{kind}: missing required {:?}",
            required.difference(&keys).collect::<Vec<_>>()
        );
    }
}

#[test]
fn a_commit_file_may_omit_its_excerpt_but_never_nulls_it() {
    let schema = schema();
    let def = &schema["$defs"]["commit"]["properties"]["files"]["items"];
    let allowed = names(&def["properties"]);
    let required = strings(&def["required"]);
    assert_eq!(def["properties"]["diff_excerpt"]["type"], "string");

    let file = CommitFile {
        path: "logo.png".into(),
        added: 0,
        removed: 0,
        status: FileStatus::Added,
        diff_excerpt: None,
    };
    let value = serde_json::to_value(&file).unwrap();
    let keys = names(&value);
    assert!(keys.is_subset(&allowed));
    assert!(required.is_subset(&keys));
    assert!(
        !keys.contains("diff_excerpt"),
        "a missing excerpt is absent, not null: {value}"
    );
}

#[test]
fn a_commit_may_omit_committed_at_but_never_nulls_it() {
    let schema = schema();
    let def = &schema["$defs"]["commit"];
    assert_eq!(def["properties"]["committed_at"]["type"], "string");
    assert_eq!(def["properties"]["committed_at"]["format"], "date-time");
    assert!(
        !strings(&def["required"]).contains("committed_at"),
        "committed_at is optional: a commit recorded before it existed is still valid"
    );

    let payload = Payload::Commit(CommitPayload {
        sha: "a".repeat(40),
        message: "feat: something".into(),
        author: "A <a@example.com>".into(),
        parents: vec![],
        files: vec![],
        secs_since_prev: None,
        committed_at: None,
    });
    let value = payload.to_value();
    assert!(
        !names(&value).contains("committed_at"),
        "a missing commit time is absent, not null: {value}"
    );
}

#[test]
fn the_file_status_and_pattern_enums_match_the_schema() {
    let schema = schema();

    let statuses = strings(
        &schema["$defs"]["commit"]["properties"]["files"]["items"]["properties"]["status"]["enum"],
    );
    let ours: BTreeSet<String> = [
        FileStatus::Added,
        FileStatus::Modified,
        FileStatus::Deleted,
        FileStatus::Renamed,
    ]
    .iter()
    .map(|s| s.as_str().to_string())
    .collect();
    assert_eq!(ours, statuses);

    let patterns = strings(&schema["$defs"]["pattern"]["properties"]["pattern"]["enum"]);
    let ours: BTreeSet<String> = [
        PatternKind::Revert,
        PatternKind::Retry,
        PatternKind::FixAfterAi,
        PatternKind::Stall,
    ]
    .iter()
    .map(|p| {
        serde_json::to_value(p)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    })
    .collect();
    assert_eq!(ours, patterns);

    let hideable = strings(&schema["$defs"]["hide"]["properties"]["target_kind"]["enum"]);
    assert_eq!(
        hideable,
        BTreeSet::from([EventKind::AiSession.as_str().to_string()])
    );
}

#[test]
fn the_schema_documents_the_hash_we_actually_compute() {
    let schema = schema();
    let description = schema["description"].as_str().unwrap();
    assert!(
        description
            .contains(r#"prev_hash + "\n" + ts + "\n" + kind + "\n" + canonical_json(payload)"#),
        "{description}"
    );
    assert!(
        description.contains("sorted keys and no whitespace"),
        "{description}"
    );
}
