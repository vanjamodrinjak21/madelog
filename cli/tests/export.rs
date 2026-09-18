mod common;

use common::*;

#[test]
fn export_writes_valid_json_with_intent_config_and_events() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.txt", "one\n");
    repo.commit_all("first");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();
    madelog::run_in(repo.root(), &["note", "wrote it down"]).unwrap();

    let out = repo.path("export.json");
    madelog::run_in(repo.root(), &["export", out.to_str().unwrap()]).unwrap();

    let raw = std::fs::read_to_string(&out).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");

    assert_eq!(v["intent"]["what"], "a small cli that records process");
    assert_eq!(v["intent"]["for_whom"], "a studio hiring juniors");
    assert_eq!(v["config"]["llm"]["provider"], "auto");
    assert!(v["config"]["project"]["name"].as_str().is_some());

    let events = v["events"].as_array().expect("events array");
    let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    // The hook writes a summary of its own once a commit is in, so the exact
    // length is not the point; the order and the shape are.
    assert_eq!(kinds[0], "intent", "{kinds:?}");
    assert_eq!(kinds[1], "commit", "{kinds:?}");
    assert_eq!(kinds.last(), Some(&"decision"), "{kinds:?}");
    for (i, e) in events.iter().enumerate() {
        assert_eq!(e["seq"], (i + 1) as i64);
        assert_eq!(e["hash"].as_str().unwrap().len(), 64);
        assert_eq!(e["prev_hash"].as_str().unwrap().len(), 64);
        assert!(e["id"].as_str().unwrap().len() >= 36);
    }
    assert_eq!(v["root"], events.last().unwrap()["hash"]);
}

#[test]
fn export_without_a_path_prints_json_to_stdout() {
    let repo = init_repo();
    init_madelog(&repo);

    let out = madelog::run_capture(repo.root(), &["export"]).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON on stdout");
    assert_eq!(v["events"].as_array().unwrap().len(), 1);
}

#[test]
fn log_json_lists_every_event() {
    let repo = init_repo();
    init_madelog(&repo);
    madelog::run_in(repo.root(), &["note", "one"]).unwrap();

    let out = madelog::run_capture(repo.root(), &["log", "--json"]).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2);

    let human = madelog::run_capture(repo.root(), &["log"]).unwrap();
    assert!(human.contains("intent"), "{human}");
    assert!(human.contains("one"), "{human}");
}
