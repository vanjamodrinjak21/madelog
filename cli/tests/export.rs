mod common;

use common::*;

#[test]
fn export_writes_valid_json_with_intent_config_and_events() {
    let repo = init_repo();
    init_pop(&repo);
    repo.write("a.txt", "one\n");
    repo.commit_all("first");
    pop::run_in(repo.root(), &["hook", "post-commit"]).unwrap();
    pop::run_in(repo.root(), &["note", "wrote it down"]).unwrap();

    let out = repo.path("export.json");
    pop::run_in(repo.root(), &["export", out.to_str().unwrap()]).unwrap();

    let raw = std::fs::read_to_string(&out).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");

    assert_eq!(v["intent"]["what"], "a small cli that records process");
    assert_eq!(v["intent"]["for_whom"], "a studio hiring juniors");
    assert_eq!(v["config"]["llm"]["provider"], "auto");
    assert_eq!(v["config"]["project"]["name"].as_str().is_some(), true);

    let events = v["events"].as_array().expect("events array");
    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["kind"], "intent");
    assert_eq!(events[1]["kind"], "commit");
    assert_eq!(events[2]["kind"], "decision");
    for (i, e) in events.iter().enumerate() {
        assert_eq!(e["seq"], (i + 1) as i64);
        assert_eq!(e["hash"].as_str().unwrap().len(), 64);
        assert_eq!(e["prev_hash"].as_str().unwrap().len(), 64);
        assert!(e["id"].as_str().unwrap().len() >= 36);
    }
    assert_eq!(v["root"], events[2]["hash"]);
}

#[test]
fn export_without_a_path_prints_json_to_stdout() {
    let repo = init_repo();
    init_pop(&repo);

    let out = pop::run_capture(repo.root(), &["export"]).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON on stdout");
    assert_eq!(v["events"].as_array().unwrap().len(), 1);
}

#[test]
fn log_json_lists_every_event() {
    let repo = init_repo();
    init_pop(&repo);
    pop::run_in(repo.root(), &["note", "one"]).unwrap();

    let out = pop::run_capture(repo.root(), &["log", "--json"]).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2);

    let human = pop::run_capture(repo.root(), &["log"]).unwrap();
    assert!(human.contains("intent"), "{human}");
    assert!(human.contains("one"), "{human}");
}
