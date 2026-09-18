mod common;

use common::*;

fn capture(repo: &TestRepo, args: &[&str]) -> anyhow::Result<String> {
    madelog::run_capture(repo.root(), args)
}

#[test]
fn verify_passes_on_an_untouched_chain() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.txt", "one\n");
    repo.commit_all("first");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();
    madelog::run_in(repo.root(), &["note", "picked a hash chain"]).unwrap();

    let out = capture(&repo, &["verify"]).expect("verify passes");
    assert!(out.contains("root "), "{out}");
    assert!(out.contains("3 events"), "{out}");

    let evs = events(&repo);
    let root = evs.last().unwrap().hash.clone();
    assert!(out.contains(&root), "root printed: {out}");
}

#[test]
fn verify_fails_after_a_payload_is_tampered_with_in_sqlite() {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.txt", "one\n");
    repo.commit_all("first");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();
    madelog::run_in(repo.root(), &["note", "an honest note"]).unwrap();

    // Someone edits the database behind madelog's back. The table is append-only,
    // so they have to remove the guard first.
    let conn = rusqlite::Connection::open(repo.path(".madelog/log.db")).unwrap();
    conn.execute_batch("DROP TRIGGER events_no_update;")
        .unwrap();
    conn.execute(
        "UPDATE events SET payload = ?1 WHERE seq = 3",
        rusqlite::params![r#"{"private":false,"text":"a flattering lie"}"#],
    )
    .unwrap();
    drop(conn);

    let err = capture(&repo, &["verify"]).unwrap_err();
    assert_eq!(exit_code(&err), Some(1), "{err:#}");
    let msg = format!("{err:#}");
    assert!(msg.contains("seq 3"), "first broken seq is reported: {msg}");
}

#[test]
fn the_events_table_rejects_updates_and_deletes() {
    let repo = init_repo();
    init_madelog(&repo);

    let conn = rusqlite::Connection::open(repo.path(".madelog/log.db")).unwrap();
    assert!(conn
        .execute("UPDATE events SET kind = 'commit' WHERE seq = 1", [])
        .is_err());
    assert!(conn
        .execute("DELETE FROM events WHERE seq = 1", [])
        .is_err());
}

#[test]
fn verify_requires_madelog_init() {
    let repo = init_repo();
    isolate_device_key();
    let err = capture(&repo, &["verify"]).unwrap_err();
    assert!(format!("{err:#}").contains("madelog init"), "{err:#}");
}
