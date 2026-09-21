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

/// `--report` is the page as data: stamped, with the status, and never more
/// than the page shows.
#[test]
fn export_report_is_the_page_as_data_with_a_stamp_and_a_status() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.txt", "one\n");
    repo.commit_all("first");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();
    madelog::run_in(repo.root(), &["note", "wrote it down"]).unwrap();
    madelog::run_in(
        repo.root(),
        &["note", "--private", "SUPERSECRETNOTE never leaves"],
    )
    .unwrap();

    // Derived status first: one commit just now means active.
    let out = madelog::run_capture(repo.root(), &["export", "--report"]).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON on stdout");
    assert_eq!(v["kind"], "madelog.report");
    assert_eq!(v["version"], 1);
    let stamp = v["exported_at"].as_str().expect("exported_at");
    assert!(stamp.ends_with('Z') && stamp.len() >= 20, "{stamp}");
    assert_eq!(v["project"]["headline"], "A small cli that records process");
    assert_eq!(v["project"]["what"], "a small cli that records process");
    assert_eq!(v["project"]["status"], "active");
    assert_eq!(v["project"]["status_source"], "derived");
    assert_eq!(v["stats"]["commits"], 1);
    assert_eq!(
        v["stats"]["decisions"], 1,
        "private decisions are not counted"
    );
    assert_eq!(v["decisions"].as_array().unwrap().len(), 1);
    assert_eq!(v["decisions"][0]["text"], "wrote it down");
    assert!(
        !out.contains("SUPERSECRETNOTE"),
        "a private note reached the report"
    );
    let highlights = v["highlights"].as_array().expect("highlights");
    assert!(
        highlights.iter().any(|h| h
            .as_str()
            .unwrap()
            .starts_with("1 commit recorded as it happened")),
        "{highlights:?}"
    );
    assert!(v["attribution"].is_null(), "no AI session, no attribution");
    assert!(v["published"].is_null());
    assert_eq!(v["record"]["verified"], false);
    assert_eq!(v["record"]["root"].as_str().unwrap().len(), 64);
    assert!(v["record"]["chained_events"].as_u64().unwrap() >= 3);
    assert!(v.get("events").is_none(), "the report is not the log");
    assert!(v.get("config").is_none(), "the report carries no config");

    // The author's word wins, and a path writes a file.
    madelog::run_in(repo.root(), &["config", "set", "project.status", "paused"]).unwrap();
    let path = repo.path("report.json");
    let said =
        madelog::run_capture(repo.root(), &["export", "--report", path.to_str().unwrap()]).unwrap();
    assert!(said.contains("wrote the report"), "{said}");
    assert!(said.contains("status paused"), "{said}");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["project"]["status"], "paused");
    assert_eq!(v["project"]["status_source"], "set");
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
