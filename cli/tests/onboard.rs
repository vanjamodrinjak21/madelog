mod common;

use std::path::Path;

use common::*;
use pop::commands::init::hook_command_for;

#[test]
fn onboard_explains_itself_and_reports_on_the_tools() {
    let repo = init_repo();
    isolate_device_key();

    let out = pop::run_capture(repo.root(), &["onboard", "--yes"]).unwrap();

    assert!(out.contains("pop records three things"), "{out}");
    assert!(out.contains("until you run `pop publish`"), "{out}");
    assert!(out.contains("pop export"), "{out}");
    for tool in ["git", "git-ai", "ollama"] {
        let line = out
            .lines()
            .find(|l| l.starts_with(tool))
            .unwrap_or_else(|| panic!("no status line for {tool} in:\n{out}"));
        assert!(line.contains("found") || line.contains("missing"), "{line}");
        if line.contains("missing") {
            assert!(line.contains("http"), "install hint for {tool}: {line}");
        }
    }
}

#[test]
fn onboard_yes_sets_the_repo_up_and_is_idempotent() {
    let repo = init_repo();
    isolate_device_key();

    pop::run_capture(repo.root(), &["onboard", "--yes"]).unwrap();

    assert!(repo.path(".pop/log.db").is_file());
    assert!(repo.path(".pop/intent.md").is_file());
    assert!(repo.path(".git/hooks/post-commit").is_file());
    assert_eq!(events(&repo).len(), 1);

    let again = pop::run_capture(repo.root(), &["onboard", "--yes"]).unwrap();
    assert!(again.contains("already set up"), "{again}");
    assert_eq!(events(&repo).len(), 1, "re-running records nothing new");
}

#[test]
fn onboard_asks_before_touching_a_repo() {
    let repo = init_repo();
    isolate_device_key();

    let mut out: Vec<u8> = Vec::new();
    pop::run_in_with(repo.root(), &["onboard"], &b"n\n"[..], &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();

    assert!(out.contains("Set up pop in this repo now?"), "{out}");
    assert!(out.contains("Nothing changed"), "{out}");
    assert!(!repo.path(".pop/log.db").exists());
}

#[test]
fn onboard_outside_a_repo_prints_the_next_two_commands() {
    isolate_device_key();
    let dir = tempfile::TempDir::new().unwrap();

    let out = pop::run_capture(dir.path(), &["onboard"]).unwrap();

    assert!(out.contains("not inside a git repository"), "{out}");
    assert!(out.lines().any(|l| l.trim() == "git init"), "{out}");
    assert!(out.lines().any(|l| l.trim() == "pop init"), "{out}");
}

#[test]
fn a_per_project_install_makes_the_hook_call_its_own_binary() {
    let repo = init_repo();
    isolate_device_key();
    let exe = repo.path(".pop/bin/pop");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "pretend binary").unwrap();

    let command = hook_command_for(repo.root(), Some(&exe));
    assert!(command.contains(".pop/bin/pop"), "{command}");
    assert_ne!(command, "pop");

    // A binary anywhere else is called by name, off PATH.
    assert_eq!(
        hook_command_for(repo.root(), Some(Path::new("/usr/local/bin/pop"))),
        "pop"
    );

    // And a per-project onboard writes that path into the hook.
    std::env::set_var("POP_EXE", &exe);
    let result = pop::run_capture(repo.root(), &["onboard", "--project"]);
    std::env::remove_var("POP_EXE");
    result.unwrap();

    let hook = std::fs::read_to_string(repo.path(".git/hooks/post-commit")).unwrap();
    assert!(hook.contains(".pop/bin/pop"), "{hook}");
    assert!(hook.contains("hook post-commit"), "{hook}");
    assert!(repo.path(".pop/log.db").is_file());
}
