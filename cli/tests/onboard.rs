mod common;

use std::path::Path;

use common::*;
use madelog::commands::init::hook_command_for;
use madelog::global::{self, GlobalConfig};

#[test]
fn onboard_explains_itself_and_reports_on_the_tools() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();

    let out = madelog::run_capture(repo.root(), &["onboard", "--yes"]).unwrap();

    assert!(out.contains("madelog records three things"), "{out}");
    assert!(out.contains("until you run `madelog publish`"), "{out}");
    assert!(out.contains("madelog export"), "{out}");
    for tool in ["git", "ollama"] {
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
fn onboard_lists_every_ai_tool_it_knows_how_to_read() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    let _home = fake_claude(&repo, chrono::Utc::now());

    let out = madelog::run_capture(repo.root(), &["onboard", "--yes"]).unwrap();

    assert!(out.contains("AI tools madelog can read:"), "{out}");
    for tool in ["claude-code", "codex", "cursor", "copilot"] {
        assert!(
            out.lines()
                .any(|l| l.trim().starts_with(&format!("{tool}:"))),
            "no line for {tool} in:\n{out}"
        );
    }
    let claude = out
        .lines()
        .find(|l| l.trim().starts_with("claude-code:"))
        .unwrap();
    assert!(claude.contains("transcripts found"), "{claude}");
    assert!(claude.contains("session"), "{claude}");
    assert!(
        out.lines().any(|l| l.contains("AI sessions:")),
        "onboard says whether sessions are being recorded:\n{out}"
    );
}

#[test]
fn nothing_madelog_prints_mentions_git_ai() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    let _home = fake_claude(&repo, chrono::Utc::now());

    let mut printed = vec![
        madelog::run_capture(repo.root(), &["onboard", "--yes"]).unwrap(),
        madelog::run_capture(repo.root(), &["status"]).unwrap(),
        madelog::run_capture(repo.root(), &["doctor"]).unwrap_or_else(|e| format!("{e:#}")),
    ];

    // Every help text the binary can print, including the nested ones.
    for args in [
        vec!["--help"],
        vec!["init", "--help"],
        vec!["onboard", "--help"],
        vec!["hook", "--help"],
        vec!["hook", "auto", "--help"],
        vec!["doctor", "--help"],
        vec!["status", "--help"],
        vec!["config", "--help"],
        vec!["publish", "--help"],
        vec!["backfill", "--help"],
        vec!["summarize", "--help"],
        vec!["open", "--help"],
    ] {
        let err = madelog::run_capture(repo.root(), &args).unwrap_err();
        printed.push(format!("{err}"));
    }

    printed.push(madelog::run_capture(repo.root(), &["init", "--yes"]).unwrap());

    for text in &printed {
        let lower = text.to_lowercase();
        assert!(
            !lower.contains("git ai"),
            "`Git AI` reached the user:\n{text}"
        );
        assert!(
            !lower.contains("git-ai"),
            "`git-ai` reached the user:\n{text}"
        );
        assert!(
            !lower.contains("usegitai"),
            "the Git AI installer reached the user:\n{text}"
        );
    }
}

#[test]
fn onboard_yes_sets_the_repo_up_and_is_idempotent() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();

    madelog::run_capture(repo.root(), &["onboard", "--yes"]).unwrap();

    assert!(repo.path(".madelog/log.db").is_file());
    assert!(repo.path(".madelog/intent.md").is_file());
    assert!(repo.path(".git/hooks/post-commit").is_file());
    assert_eq!(events(&repo).len(), 1);

    let md = std::fs::read_to_string(repo.path(".madelog/intent.md")).unwrap();
    for flag in ["--yes", "--project"] {
        let again = madelog::run_capture(repo.root(), &["onboard", flag]).unwrap();
        assert!(again.contains("already set up"), "{flag}: {again}");
        assert_eq!(
            events(&repo).len(),
            1,
            "{flag} records nothing new on a repo that is already set up"
        );
        assert_eq!(
            std::fs::read_to_string(repo.path(".madelog/intent.md")).unwrap(),
            md,
            "{flag} leaves intent.md alone"
        );
    }
}

#[test]
fn onboard_asks_before_touching_a_repo() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();

    // "no" to every repository, then "no" to this one.
    let mut out: Vec<u8> = Vec::new();
    madelog::run_in_with(repo.root(), &["onboard"], &b"n\nn\n"[..], &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();

    assert!(
        out.contains("Record every repository you commit in?"),
        "{out}"
    );
    assert!(out.contains("Set up madelog in this repo now?"), "{out}");
    assert!(out.contains("Nothing changed"), "{out}");
    assert!(!repo.path(".madelog/log.db").exists());
    assert!(!global::is_enabled(), "a no is a no");
}

#[test]
fn saying_yes_to_every_repository_installs_the_global_hooks() {
    let (_lock, machine) = isolate_machine();
    let repo = init_repo();

    // every repository? yes. set up new ones by itself? yes.
    // record their AI sessions? yes. set this one up now? no.
    let mut out: Vec<u8> = Vec::new();
    madelog::run_in_with(repo.root(), &["onboard"], &b"y\ny\ny\nn\n"[..], &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();

    assert!(global::is_enabled(), "{out}");
    let hooks = machine.path().join("hooks");
    for name in ["post-commit", "post-merge", "post-rewrite"] {
        let body = std::fs::read_to_string(hooks.join(name)).unwrap();
        assert!(body.contains(&format!("hook auto {name}")), "{body}");
    }
    assert_eq!(
        GlobalConfig::load(),
        GlobalConfig {
            auto_init: true,
            record_ai_sessions: true,
        }
    );
    assert!(out.contains("in every repository from now on"), "{out}");

    // Running it again says so rather than asking twice.
    let again = madelog::run_capture(repo.root(), &["onboard", "--yes"]).unwrap();
    assert!(again.contains("Every repository: on"), "{again}");
}

#[test]
fn a_hooks_path_somebody_else_set_is_chained_not_replaced() {
    let (_lock, machine) = isolate_machine();
    let theirs = machine.path().join("their-hooks");
    std::fs::create_dir_all(&theirs).unwrap();
    std::fs::write(theirs.join("post-commit"), "#!/bin/sh\necho theirs\n").unwrap();
    global::set_hooks_path(&theirs).unwrap();

    let enabled = global::enable("madelog").unwrap();

    assert_eq!(enabled.chained.as_deref(), Some(theirs.as_path()));
    let body = std::fs::read_to_string(enabled.dir.join("post-commit")).unwrap();
    assert!(body.contains("madelog hook auto post-commit"), "{body}");
    assert!(body.contains("their-hooks/post-commit"), "{body}");
    assert!(global::is_enabled());

    // And a second run leaves our own directory alone rather than chaining
    // to itself.
    let again = global::enable("madelog").unwrap();
    assert_eq!(again.chained, None);
    assert!(!again.changed);
    let body = std::fs::read_to_string(again.dir.join("post-commit")).unwrap();
    assert!(!body.contains("their-hooks"), "{body}");
}

#[test]
fn onboard_outside_a_repo_prints_the_next_two_commands() {
    let (_lock, _machine) = isolate_machine();
    let dir = tempfile::TempDir::new().unwrap();

    let mut out: Vec<u8> = Vec::new();
    madelog::run_in_with(dir.path(), &["onboard"], &b"n\n"[..], &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();

    assert!(out.contains("not inside a git repository"), "{out}");
    assert!(out.lines().any(|l| l.trim() == "git init"), "{out}");
    assert!(out.lines().any(|l| l.trim() == "madelog init"), "{out}");
}

#[test]
fn a_per_project_install_makes_the_hook_call_its_own_binary() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    let exe = repo.path(".madelog/bin/madelog");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "pretend binary").unwrap();

    let command = hook_command_for(repo.root(), Some(&exe));
    assert!(command.contains(".madelog/bin/madelog"), "{command}");
    assert_ne!(command, "madelog");

    // A binary anywhere else is called by name, off PATH.
    assert_eq!(
        hook_command_for(repo.root(), Some(Path::new("/usr/local/bin/madelog"))),
        "madelog"
    );

    // And a per-project onboard writes that path into the hook.
    std::env::set_var("MADELOG_EXE", &exe);
    let result = madelog::run_capture(repo.root(), &["onboard", "--project"]);
    std::env::remove_var("MADELOG_EXE");
    result.unwrap();

    let hook = std::fs::read_to_string(repo.path(".git/hooks/post-commit")).unwrap();
    assert!(hook.contains(".madelog/bin/madelog"), "{hook}");
    assert!(hook.contains("hook post-commit"), "{hook}");
    assert!(repo.path(".madelog/log.db").is_file());
    assert!(
        !global::is_enabled(),
        "a per-project install stays out of the machine's git config"
    );
}

#[test]
fn onboard_installs_the_claude_skill_when_claude_code_is_here() {
    let (_lock, _machine) = isolate_machine();
    let repo = init_repo();
    let claude = tempfile::TempDir::new().unwrap();
    std::env::set_var("MADELOG_CLAUDE_HOME", claude.path());

    let out = madelog::run_capture(repo.root(), &["onboard", "--yes"]).unwrap();
    let skill = claude.path().join("skills/madelog/SKILL.md");
    assert!(out.contains("Claude Code skill installed"), "{out}");
    assert_eq!(
        std::fs::read_to_string(&skill).unwrap(),
        madelog::skill::SKILL
    );

    let again = madelog::run_capture(repo.root(), &["onboard", "--yes"]).unwrap();
    assert!(again.contains("Claude Code skill: up to date."), "{again}");

    // No ~/.claude, no skill and no line about it.
    let missing = claude.path().join("not-here");
    std::env::set_var("MADELOG_CLAUDE_HOME", &missing);
    let out = madelog::run_capture(repo.root(), &["onboard", "--yes"]).unwrap();
    assert!(!out.contains("Claude Code skill"), "{out}");
    assert!(!missing.exists());
    reset_ai_tools();
}
