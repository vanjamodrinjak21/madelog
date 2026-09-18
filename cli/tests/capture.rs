//! The capture layer against fixtures built from real log shapes.
//!
//! Every test here points madelog's adapters at a fake tool home, which is one
//! environment variable per tool and therefore one per process: they take the
//! lock in `common` and go one at a time.

mod common;

use std::path::Path;

use chrono::{Duration, Utc};
use common::*;
use madelog::capture::{self, Adapter, CapturedSession, CommitDiff, CommitFileDiff, WriteKind};
use madelog::gitrepo::{ExcerptFilter, Repo};

fn window() -> (chrono::DateTime<Utc>, chrono::DateTime<Utc>) {
    let now = Utc::now();
    (now - Duration::hours(1), now + Duration::hours(1))
}

fn claude_sessions(repo: &TestRepo) -> Vec<CapturedSession> {
    let (since, until) = window();
    capture::claude::ClaudeCode.sessions(repo.root(), since, until)
}

// ---------------------------------------------------------------------------
// Claude Code
// ---------------------------------------------------------------------------

#[test]
fn claude_reads_one_session_out_of_history_transcript_and_subagent() {
    let _lock = capture_lock();
    let repo = init_repo();
    let _home = fake_claude(&repo, Utc::now());

    let sessions = claude_sessions(&repo);
    assert_eq!(sessions.len(), 1, "{sessions:#?}");
    let s = &sessions[0];

    assert_eq!(s.tool, "claude-code");
    assert_eq!(s.session_id, FIXTURE_CLAUDE_SESSION);
    assert_eq!(
        s.prompt.as_deref(),
        Some("write a parser for the log lines"),
        "the first real prompt, whitespace collapsed"
    );
    assert_eq!(
        s.prompt_count, 2,
        "system reminders and tool results are not prompts"
    );
    assert_eq!(
        s.model.as_deref(),
        Some("claude-sonnet-4-5-20250929"),
        "the mode of the assistant models, not the subagent's one-off"
    );
    assert_eq!(s.title.as_deref(), Some("Log line parser"));
    assert_eq!(s.lines_added_hint, Some(12));
    assert!(s.started <= s.ended);

    // src/parser.rs from the main transcript, src/lib.rs from the subagent.
    let paths: Vec<String> = s
        .writes
        .iter()
        .map(|w| capture::relative(repo.root(), &w.path).unwrap())
        .collect();
    assert_eq!(
        paths,
        [".env", "src/lib.rs", "src/parser.rs"],
        "the adapter reports what it read; `collect` is where .env is dropped"
    );

    let parser = s
        .writes
        .iter()
        .find(|w| w.path.ends_with("parser.rs"))
        .unwrap();
    assert_eq!(parser.kind, WriteKind::Create);
    assert_eq!(parser.lines[0], "pub fn parse(line: &str) -> Option<u32> {");
    assert_eq!(parser.lines.last().unwrap(), "}");

    let lib = s
        .writes
        .iter()
        .find(|w| w.path.ends_with("lib.rs"))
        .unwrap();
    assert_eq!(lib.kind, WriteKind::Update);
    assert!(
        lib.lines.contains(&"pub mod parser;".to_string()),
        "{lib:?}"
    );
    assert_eq!(
        lib.lines.iter().filter(|l| *l == "pub mod parser;").count(),
        1,
        "newString and the patch describe one line, not two"
    );
}

#[test]
fn claude_never_records_a_failed_write_tool_output_or_a_file_outside_the_repo() {
    let _lock = capture_lock();
    let repo = init_repo();
    let _home = fake_claude(&repo, Utc::now());

    let sessions = claude_sessions(&repo);
    let all: Vec<String> = sessions
        .iter()
        .flat_map(|s| s.writes.iter())
        .map(|w| w.path.display().to_string())
        .collect();

    assert!(!all.iter().any(|p| p.ends_with("never.rs")), "{all:?}");
    assert!(!all.iter().any(|p| p.contains("elsewhere.rs")), "{all:?}");

    let text = format!("{sessions:?}");
    assert!(
        !text.contains("test result: ok"),
        "tool stdout is never kept"
    );
    assert!(
        !text.contains("this line must never be recorded"),
        "a failed write leaves nothing behind"
    );
}

#[test]
fn claude_ignores_sessions_that_belong_to_another_repository() {
    let _lock = capture_lock();
    let repo = init_repo();
    let _home = fake_claude(&repo, Utc::now());

    let other = init_repo();
    assert!(claude_sessions(&other).is_empty());
}

#[test]
fn claude_ignores_a_session_outside_the_window() {
    let _lock = capture_lock();
    let repo = init_repo();
    let _home = fake_claude(&repo, Utc::now() - Duration::days(3));

    let (since, until) = window();
    assert!(capture::claude::ClaudeCode
        .sessions(repo.root(), since, until)
        .is_empty());

    // Widen the window and the same session is there again.
    let found = capture::claude::ClaudeCode.sessions(
        repo.root(),
        Utc::now() - Duration::days(10),
        Utc::now(),
    );
    assert_eq!(found.len(), 1);
}

#[test]
fn a_malformed_line_costs_the_line_and_not_the_file() {
    let _lock = capture_lock();
    let repo = init_repo();
    let home = fake_claude(&repo, Utc::now());

    // Both fixtures already carry junk lines; prove the session still reads.
    let history = std::fs::read_to_string(home.path().join("history.jsonl")).unwrap();
    assert!(history
        .lines()
        .any(|l| serde_json::from_str::<serde_json::Value>(l).is_err()));
    assert_eq!(claude_sessions(&repo).len(), 1);
}

// ---------------------------------------------------------------------------
// Codex
// ---------------------------------------------------------------------------

#[test]
fn codex_reads_a_rollout_by_its_session_meta_cwd() {
    let _lock = capture_lock();
    let repo = init_repo();
    let _home = fake_codex(&repo, Utc::now());

    let (since, until) = window();
    let sessions = capture::codex::Codex.sessions(repo.root(), since, until);
    assert_eq!(sessions.len(), 1, "the other rollout is another project");
    let s = &sessions[0];

    assert_eq!(s.tool, "codex");
    assert_eq!(s.session_id, "019a1f7c-0000-7000-8000-000000000001");
    assert_eq!(s.model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(s.prompt.as_deref(), Some("add the codex reader"));
    assert_eq!(s.prompt_count, 1);

    let paths: Vec<String> = s
        .writes
        .iter()
        .map(|w| capture::relative(repo.root(), &w.path).unwrap())
        .collect();
    assert_eq!(paths, ["src/codex.rs", "src/main.rs"]);

    let added = s
        .writes
        .iter()
        .find(|w| w.path.ends_with("codex.rs"))
        .unwrap();
    assert_eq!(added.kind, WriteKind::Create);
    assert_eq!(added.lines[0], "pub fn run() {");

    let updated = s
        .writes
        .iter()
        .find(|w| w.path.ends_with("main.rs"))
        .unwrap();
    assert_eq!(updated.kind, WriteKind::Update);
    assert_eq!(updated.lines, ["    codex::run();"], "only the + lines");
}

#[test]
fn codex_says_nothing_about_a_write_done_through_the_shell() {
    let _lock = capture_lock();
    let repo = init_repo();
    let _home = fake_codex(&repo, Utc::now());

    let (since, until) = window();
    let sessions = capture::codex::Codex.sessions(repo.root(), since, until);
    let paths: Vec<String> = sessions
        .iter()
        .flat_map(|s| s.writes.iter())
        .map(|w| w.path.display().to_string())
        .collect();
    assert!(
        !paths.iter().any(|p| p.ends_with("notes.txt")),
        "a heredoc leaves no FileChange, and madelog does not guess: {paths:?}"
    );
}

// ---------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------

#[test]
fn cursor_records_that_a_session_happened_and_claims_nothing_more() {
    let _lock = capture_lock();
    let repo = init_repo();
    let _home = fake_cursor(&repo, Utc::now());

    let (since, until) = window();
    let sessions = capture::cursor::Cursor.sessions(repo.root(), since, until);
    assert_eq!(sessions.len(), 1, "the other workspace is another project");
    let s = &sessions[0];

    assert_eq!(s.tool, "cursor");
    assert_eq!(s.session_id, "cmp-7781-0000-4000-8000-000000000001");
    assert_eq!(s.model.as_deref(), Some("claude-4.5-sonnet"));
    assert_eq!(s.lines_added_hint, Some(41));
    assert_eq!(s.prompt, None, "the bodies are encrypted");
    assert!(s.writes.is_empty(), "there is no verbatim text to have");
    assert!(s.ended > s.started);
}

#[test]
fn the_cursor_note_says_why_there_is_no_prompt() {
    let _lock = capture_lock();
    let repo = init_repo();
    let _home = fake_cursor(&repo, Utc::now());

    let detection = capture::cursor::Cursor.detect();
    assert!(detection.installed);
    assert_eq!(
        detection.note,
        "Cursor keeps conversation bodies encrypted; madelog records that a session happened"
    );
}

// ---------------------------------------------------------------------------
// Copilot
// ---------------------------------------------------------------------------

#[test]
fn copilot_is_detected_and_read_by_nobody() {
    let _lock = capture_lock();
    let repo = init_repo();
    let (since, until) = window();
    assert!(capture::copilot::Copilot
        .sessions(repo.root(), since, until)
        .is_empty());
    assert!(
        capture::copilot::Copilot
            .detect()
            .note
            .contains("not supported")
            || !capture::copilot::Copilot.detect().installed
    );
}

// ---------------------------------------------------------------------------
// redaction at capture time
// ---------------------------------------------------------------------------

#[test]
fn an_ignored_file_never_becomes_a_write() {
    let _lock = capture_lock();
    let repo = init_repo();
    repo.write(".gitignore", ".env\n");
    repo.write(".env", "TOKEN=x\n");
    repo.commit_all("chore: ignore the env file");
    init_madelog(&repo);
    let _home = fake_claude(&repo, Utc::now());

    let git = Repo::discover(repo.root()).unwrap();
    let config = madelog::config::Config::default();
    let (since, until) = window();
    let sessions = capture::collect(&git, &config, since, until);

    let paths: Vec<String> = sessions
        .iter()
        .flat_map(|s| s.writes.iter())
        .filter_map(|w| capture::relative(repo.root(), &w.path))
        .collect();
    assert!(paths.contains(&"src/parser.rs".to_string()), "{paths:?}");
    assert!(
        !paths.iter().any(|p| p == ".env"),
        "a file git ignores is not madelog's to record: {paths:?}"
    );
}

#[test]
fn a_redact_pattern_drops_a_matching_prompt_whole_and_a_matching_path() {
    let _lock = capture_lock();
    let repo = init_repo();
    repo.write(".gitignore", ".env\n");
    repo.commit_all("chore: ignore the env file");
    init_madelog(&repo);
    let _home = fake_claude(&repo, Utc::now());

    let git = Repo::discover(repo.root()).unwrap();
    let mut config = madelog::config::Config::default();
    config.privacy.redact = vec!["parser".to_string()];
    let (since, until) = window();
    let sessions = capture::collect(&git, &config, since, until);

    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].prompt, None,
        "the prompt says `parser`, so it goes whole rather than masked"
    );
    let paths: Vec<String> = sessions[0]
        .writes
        .iter()
        .filter_map(|w| capture::relative(repo.root(), &w.path))
        .collect();
    assert_eq!(paths, ["src/lib.rs"], "src/parser.rs matched the pattern");
}

#[test]
fn redaction_leaves_a_session_that_matches_nothing_alone() {
    let _lock = capture_lock();
    let repo = init_repo();
    repo.write(".gitignore", ".env\n");
    repo.commit_all("chore: ignore the env file");
    init_madelog(&repo);
    let _home = fake_claude(&repo, Utc::now());

    let git = Repo::discover(repo.root()).unwrap();
    let mut config = madelog::config::Config::default();
    config.privacy.redact = vec!["^secrets/".to_string()];
    let (since, until) = window();
    let sessions = capture::collect(&git, &config, since, until);

    assert_eq!(
        sessions[0].prompt.as_deref(),
        Some("write a parser for the log lines")
    );
    assert_eq!(sessions[0].writes.len(), 2);
}

#[test]
fn collect_gathers_every_tool_in_one_list() {
    let _lock = capture_lock();
    let repo = init_repo();
    init_madelog(&repo);
    let _claude = fake_claude(&repo, Utc::now());
    let _codex = fake_codex(&repo, Utc::now());
    let _cursor = fake_cursor(&repo, Utc::now());

    let git = Repo::discover(repo.root()).unwrap();
    let config = madelog::config::Config::default();
    let (since, until) = window();
    let mut tools: Vec<String> = capture::collect(&git, &config, since, until)
        .into_iter()
        .map(|s| s.tool)
        .collect();
    tools.sort();
    assert_eq!(tools, ["claude-code", "codex", "cursor"]);
}

// ---------------------------------------------------------------------------
// attribution against a real commit
// ---------------------------------------------------------------------------

#[test]
fn attribution_counts_the_lines_the_commit_really_took_from_the_session() {
    let _lock = capture_lock();
    let repo = init_repo();
    let source = fixture_parser_source();
    repo.write("src/parser.rs", &source);
    let sha = repo.commit_all("feat: parse log lines");
    init_madelog(&repo);
    let _home = fake_claude(&repo, Utc::now());

    let git = Repo::discover(repo.root()).unwrap();
    let diff = git.commit_diff(&sha).unwrap();
    let (since, until) = window();
    let sessions = capture::collect(&git, &madelog::config::Config::default(), since, until);

    let payloads = capture::attribute(&sessions, &diff, repo.root(), &[]);
    assert_eq!(payloads.len(), 1);
    let p = &payloads[0];
    assert_eq!(p.tool, "claude-code");
    assert!(p.accepted);
    assert_eq!(p.response_excerpt, None, "the reply is never stored");

    let file = p.files.iter().find(|f| f.path == "src/parser.rs").unwrap();
    let meaningful = source.lines().filter(|l| !l.trim().is_empty()).count() as u64;
    assert_eq!(file.ai_lines, meaningful);
    assert_eq!(file.ai_lines_modified_by_human, 0);

    // src/lib.rs was written by the subagent but is not in this commit.
    assert!(p.files.iter().all(|f| f.path != "src/lib.rs"));
}

#[test]
fn a_line_the_person_rewrote_before_committing_is_counted_as_reworked() {
    let _lock = capture_lock();
    let repo = init_repo();
    let source = fixture_parser_source();
    let changed = source.replace("line.trim()", "line.trim_start()");
    assert_ne!(changed, source);
    repo.write("src/parser.rs", &changed);
    let sha = repo.commit_all("feat: parse log lines, my way");
    init_madelog(&repo);
    let _home = fake_claude(&repo, Utc::now());

    let git = Repo::discover(repo.root()).unwrap();
    let diff = git.commit_diff(&sha).unwrap();
    let (since, until) = window();
    let sessions = capture::collect(&git, &madelog::config::Config::default(), since, until);
    let payloads = capture::attribute(&sessions, &diff, repo.root(), &[]);

    let file = payloads[0]
        .files
        .iter()
        .find(|f| f.path == "src/parser.rs")
        .unwrap();
    let meaningful = source.lines().filter(|l| !l.trim().is_empty()).count() as u64;
    assert_eq!(file.ai_lines, meaningful - 1);
    assert_eq!(file.ai_lines_modified_by_human, 1);
    assert!(payloads[0].accepted, "most of it did survive");
}

#[test]
fn a_commit_diff_reads_added_lines_and_the_committed_contents() {
    let repo = init_repo();
    repo.write("a.txt", "one\ntwo\n");
    repo.commit_all("first");
    repo.write("a.txt", "one\ntwo\nthree\n");
    let sha = repo.commit_all("second");

    let git = Repo::discover(repo.root()).unwrap();
    let diff = git.commit_diff(&sha).unwrap();
    let file = diff.file("a.txt").unwrap();
    assert_eq!(file.added, ["three"], "only what the commit added");
    assert_eq!(file.content, ["one", "two", "three"]);
}

#[test]
fn a_deleted_file_is_not_a_file_a_commit_added() {
    let repo = init_repo();
    repo.write("gone.txt", "bye\n");
    repo.commit_all("first");
    std::fs::remove_file(repo.path("gone.txt")).unwrap();
    let sha = repo.commit_all("remove it");

    let git = Repo::discover(repo.root()).unwrap();
    assert!(git.commit_diff(&sha).unwrap().file("gone.txt").is_none());
}

#[test]
fn a_session_whose_work_is_not_in_this_commit_waits_for_the_one_that_has_it() {
    let _lock = capture_lock();
    let repo = init_repo();
    repo.write("README.md", "# hi\n");
    let sha = repo.commit_all("docs: readme");
    init_madelog(&repo);
    let _home = fake_claude(&repo, Utc::now());

    let git = Repo::discover(repo.root()).unwrap();
    let diff = git.commit_diff(&sha).unwrap();
    let (since, until) = window();
    let sessions = capture::collect(&git, &madelog::config::Config::default(), since, until);
    let payloads = capture::attribute(&sessions, &diff, repo.root(), &[]);

    assert!(
        payloads.is_empty(),
        "the session wrote src/parser.rs; this commit is a README: {payloads:#?}"
    );
}

#[test]
fn redaction_is_applied_before_anything_is_attributed() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let filter = ExcerptFilter::new(&["token".to_string()]).unwrap();
    let repo = {
        git2::Repository::init(root).unwrap();
        Repo::discover(root).unwrap()
    };

    let now = Utc::now();
    let session = CapturedSession {
        tool: "claude-code".into(),
        session_id: "s".into(),
        model: None,
        started: now,
        ended: now,
        prompt: Some("rotate the token".into()),
        prompt_count: 1,
        title: None,
        writes: vec![madelog::capture::Write {
            path: root.join("token.rs"),
            lines: vec!["let t = 1;".into()],
            kind: WriteKind::Create,
        }],
        lines_added_hint: None,
    };

    let redacted = capture::redact(session, &repo, root, &filter);
    assert_eq!(redacted.prompt, None);
    assert!(redacted.writes.is_empty());

    let diff = CommitDiff {
        files: vec![CommitFileDiff {
            path: "token.rs".into(),
            added: vec!["let t = 1;".into()],
            content: vec!["let t = 1;".into()],
        }],
    };
    let payloads = capture::attribute(&[redacted], &diff, root, &[]);
    assert!(payloads[0].files.is_empty(), "nothing left to attribute");
}

#[test]
fn every_tool_reports_itself_for_a_repository() {
    let _lock = capture_lock();
    let repo = init_repo();
    init_madelog(&repo);
    let _home = fake_claude(&repo, Utc::now());

    let reports = capture::report(Some(repo.root()));
    let claude = reports.iter().find(|r| r.name == "claude-code").unwrap();
    assert!(claude.installed);
    assert_eq!(claude.sessions, Some(1));
    assert!(
        claude.line().contains("1 session touches this repo"),
        "{}",
        claude.line()
    );

    let elsewhere = capture::report(Some(Path::new("/definitely/not/a/repo")));
    let claude = elsewhere.iter().find(|r| r.name == "claude-code").unwrap();
    assert_eq!(claude.sessions, Some(0));
    assert!(claude.line().contains("no sessions touch this repo"));
}
