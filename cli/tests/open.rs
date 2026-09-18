//! `madelog open` — the page, in a browser, without a server.

mod common;

use common::*;

fn repo_with_a_commit() -> TestRepo {
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.rs", "// a\n");
    repo.commit_all("first");
    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();
    repo
}

#[test]
fn open_print_renders_the_page_and_prints_where_it_is() {
    let repo = repo_with_a_commit();
    // The hook renders the page as it records; take it away again so this
    // test is about `madelog open` and nothing else.
    std::fs::remove_file(repo.path(".madelog/site/index.html")).unwrap();
    assert!(!repo.path(".madelog/site/index.html").exists());

    let out = madelog::run_capture(repo.root(), &["open", "--print"]).unwrap();

    let printed = out.trim();
    assert!(printed.ends_with(".madelog/site/index.html"), "{out}");
    assert!(
        repo.path(".madelog/site/index.html").exists(),
        "a missing page is rendered first: {out}"
    );
    let html = std::fs::read_to_string(printed).unwrap();
    assert!(
        html.starts_with("<!doctype html>"),
        "{}",
        &html[..40.min(html.len())]
    );
}

#[test]
fn open_print_keeps_a_page_that_is_already_there() {
    let repo = repo_with_a_commit();
    madelog::run_capture(repo.root(), &["publish", "--local", "--yes"]).unwrap();
    let path = repo.path(".madelog/site/index.html");
    std::fs::write(&path, "<!doctype html><html>mine</html>").unwrap();

    madelog::run_capture(repo.root(), &["open", "--print"]).unwrap();

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "<!doctype html><html>mine</html>",
        "--print never re-renders a page that exists"
    );
}

#[test]
fn open_hands_the_page_to_the_desktop_opener() {
    let repo = repo_with_a_commit();
    let marker = repo.path("opened.txt");
    // A stand-in for `open` / `xdg-open` / `cmd /C start`.
    let script = repo.path("opener.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nprintf '%s' \"$1\" > {}\n", marker.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::env::set_var(
        madelog::commands::open::OPENER_ENV,
        script.display().to_string(),
    );

    let out = madelog::run_capture(repo.root(), &["open"]).unwrap();

    std::env::remove_var(madelog::commands::open::OPENER_ENV);
    assert!(out.contains("opened"), "{out}");
    let handed = std::fs::read_to_string(&marker).expect("the opener ran");
    assert!(handed.ends_with(".madelog/site/index.html"), "{handed}");
}

#[test]
fn open_requires_madelog_init() {
    let repo = init_repo();
    isolate_device_key();
    let err = madelog::run_in(repo.root(), &["open", "--print"]).unwrap_err();
    assert!(format!("{err:#}").contains("madelog init"), "{err:#}");
}

#[test]
fn open_renders_the_page_again_when_the_log_has_moved_on() {
    let repo = repo_with_a_commit();
    let path = repo.path(".madelog/site/index.html");
    std::fs::write(&path, "<!doctype html><html>stale</html>").unwrap();

    // A note is an event, and it lands after the page was written.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    madelog::run_capture(repo.root(), &["note", "chose sqlite over postgres"]).unwrap();

    let status = madelog::run_capture(repo.root(), &["status"]).unwrap();
    assert!(status.contains("page older than log"), "{status}");

    madelog::run_capture(repo.root(), &["open", "--print"]).unwrap();
    let html = std::fs::read_to_string(&path).unwrap();
    assert!(!html.contains("stale"), "the page was rendered again");
    assert!(
        html.contains("chose sqlite over postgres"),
        "and it knows about the note"
    );

    // Now the page is newer than the log again.
    let status = madelog::run_capture(repo.root(), &["status"]).unwrap();
    assert!(!status.contains("page older than log"), "{status}");
}
