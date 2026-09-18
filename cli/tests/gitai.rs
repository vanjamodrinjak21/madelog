//! The Git AI reader: parsing real note bodies and mapping them to events.
//!
//! The fixtures in `tests/fixtures/gitai/` are note blobs of the shape
//! `git ai show <rev>` prints, described in
//! `docs/superpowers/specs/gitai-format.md`.

mod common;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use common::*;
use madelog::event::{AiSessionPayload, EventKind};
use madelog::gitai::note::{self, Route};
use madelog::gitai::{self, prompt};
use madelog::gitrepo::ExcerptFilter;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/gitai")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Keep the transcript lookup away from the developer's own
/// `~/.claude/projects` for every test in this binary.
fn isolate_claude_projects() -> &'static Path {
    static DIR: OnceLock<TempDir> = OnceLock::new();
    let dir = DIR.get_or_init(|| TempDir::new().expect("temp projects dir"));
    std::env::set_var(prompt::PROJECTS_DIR_ENV, dir.path());
    dir.path()
}

/// Put a device key in the isolated config dir before any `madelog init` runs.
///
/// `DeviceKey::load_or_create` is check-then-write, so two `madelog init` calls
/// in parallel can read a half-written key. Planting a valid one first means
/// neither of this binary's inits ever has to create it.
fn plant_device_key() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        isolate_device_key();
        let dir = std::env::var_os(madelog::keys::CONFIG_DIR_ENV).expect("isolated config dir");
        let path = Path::new(&dir).join("device.key");
        if !path.exists() {
            std::fs::write(&path, format!("{}\n", "11".repeat(32))).unwrap();
        }
    });
}

/// Attach `blob` to `sha` on `refs/notes/ai`, the way Git AI's daemon does.
fn write_note(repo: &TestRepo, sha: &str, blob: &str) {
    let git = git2::Repository::open(repo.root()).unwrap();
    let sig = git2::Signature::now("Git AI", "daemon@usegitai.com").unwrap();
    let oid = git2::Oid::from_str(sha).unwrap();
    git.note(&sig, &sig, Some(gitai::NOTES_REF), oid, blob, false)
        .unwrap();
}

fn sessions(repo: &TestRepo, sha: &str) -> Vec<AiSessionPayload> {
    let git = git2::Repository::open(repo.root()).unwrap();
    gitai::sessions_for_commit(&git, sha).unwrap()
}

/// Write a Claude Code transcript where the reader will look for it.
fn write_transcript(base: &Path, workdir: &Path, session_id: &str, lines: &[&str]) -> PathBuf {
    let dir = base.join(prompt::encode_project_dir(workdir));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session_id}.jsonl"));
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    path
}

fn user_line(text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": text }
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// the parser
// ---------------------------------------------------------------------------

#[test]
fn parses_the_documented_single_session_note() {
    let parsed = note::parse(&fixture("single-claude-session.note")).unwrap();

    assert_eq!(parsed.schema_version, "authorship/3.0.0");
    assert_eq!(parsed.git_ai_version.as_deref(), Some("development:1.6.17"));
    assert_eq!(
        parsed.base_commit_sha.as_deref(),
        Some("ff11ab3fe24d01149f4be8bfcc84b4ae6fb58b38")
    );
    assert!(parsed.prompts.is_empty());
    assert_eq!(parsed.humans.len(), 1);
    assert_eq!(parsed.sessions.len(), 1);

    assert_eq!(parsed.files.len(), 1);
    assert_eq!(parsed.files[0].path, "src/daemon.rs");
    assert_eq!(parsed.files[0].entries.len(), 3);

    let attribution = parsed.attribution();
    assert_eq!(
        attribution.len(),
        2,
        "one session, one human: {attribution:?}"
    );

    // Both traces of the same session collapse onto the session key.
    let session = &attribution[0];
    assert_eq!(session.route, Route::Session);
    assert_eq!(session.key, "s_2c8a703bb1fd71");
    assert_eq!(session.total, 16);
    assert_eq!(session.files.len(), 1);
    assert_eq!(session.files[0].path, "src/daemon.rs");
    assert_eq!(session.files[0].lines, 16);

    let human = &attribution[1];
    assert_eq!(human.route, Route::Human);
    assert_eq!(human.key, "h_ea2010baa7a42b");
    assert_eq!(human.total, 5);

    assert!(parsed.unknown_keys().is_empty());
}

#[test]
fn later_entries_override_earlier_lines() {
    let parsed = note::parse(&fixture("overlapping-ranges.note")).unwrap();
    let attribution = parsed.attribution();

    // 1-10, then 5-8 taken by a second session, then 9 taken by a human.
    let lines = |key: &str| {
        attribution
            .iter()
            .find(|a| a.key == key)
            .unwrap_or_else(|| panic!("{key} missing from {attribution:?}"))
            .total
    };
    assert_eq!(lines("s_1111111111aaaa"), 5, "1-4 and 10 survive");
    assert_eq!(lines("s_2222222222bbbb"), 4);
    assert_eq!(lines("h_3333333333cccc"), 1);
    assert_eq!(attribution.iter().map(|a| a.total).sum::<u64>(), 10);
}

#[test]
fn a_quoted_path_with_whitespace_is_unquoted() {
    let parsed = note::parse(&fixture("quoted-path.note")).unwrap();
    let paths: Vec<&str> = parsed.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["docs/design notes.md", "plain/file.txt"]);

    let attribution = parsed.attribution();
    assert_eq!(attribution.len(), 1);
    assert_eq!(attribution[0].total, 4);
    assert_eq!(
        attribution[0]
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.lines))
            .collect::<Vec<_>>(),
        [("docs/design notes.md", 3), ("plain/file.txt", 1)]
    );
}

#[test]
fn a_note_without_a_sessions_map_still_parses() {
    let parsed = note::parse(&fixture("no-sessions-map.note")).unwrap();
    assert!(parsed.sessions.is_empty());
    assert!(parsed.humans.is_empty());
    assert_eq!(parsed.prompts.len(), 1);
    assert_eq!(parsed.git_ai_version, None);

    // The session key has nowhere to resolve to, so it is unknown, not fatal.
    assert_eq!(parsed.unknown_keys(), ["s_ff00ff00ff00ff"]);
}

#[test]
fn keys_that_match_no_map_are_counted_as_unknown() {
    let parsed = note::parse(&fixture("unknown-key.note")).unwrap();
    assert_eq!(
        parsed.unknown_keys(),
        ["s_cherrypickedaa", "h_notinthismapxx", "9998887"]
    );
    assert_eq!(parsed.unknown_lines(), 6);
    // The one key it does know is untouched by its foreign neighbours.
    let known = parsed
        .attribution()
        .into_iter()
        .find(|a| a.key == "s_1234567890abcd")
        .unwrap();
    assert_eq!(known.total, 6);
}

#[test]
fn malformed_json_after_the_separator_is_an_error() {
    let err = note::parse(&fixture("malformed-json.note")).unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("Git AI note"), "{text}");
}

#[test]
fn a_blob_without_a_separator_is_an_error() {
    let err = note::parse("src/main.rs\n  s_aa::t_bb 1-2\n").unwrap_err();
    assert!(format!("{err:#}").contains("---"), "{err:#}");
}

#[test]
fn ranges_are_inclusive_and_ascending() {
    let err =
        note::parse("a.rs\n  s_a 9-2\n---\n{\"schema_version\":\"authorship/3.0.0\"}").unwrap_err();
    assert!(format!("{err:#}").contains("9-2"), "{err:#}");
}

#[test]
fn keys_route_by_prefix() {
    assert_eq!(
        note::route("s_2c8a703bb1fd71::t_d92697dfcc1b35"),
        (Route::Session, "s_2c8a703bb1fd71")
    );
    assert_eq!(note::route("s_short"), (Route::Session, "s_short"));
    assert_eq!(
        note::route("h_ea2010baa7a42b"),
        (Route::Human, "h_ea2010baa7a42b")
    );
    assert_eq!(note::route("4f3a9b1"), (Route::Prompt, "4f3a9b1"));
}

// ---------------------------------------------------------------------------
// mapping to events
// ---------------------------------------------------------------------------

#[test]
fn a_repository_without_a_notes_ref_yields_no_sessions() {
    isolate_claude_projects();
    let repo = init_repo();
    repo.write("a.txt", "one\n");
    let sha = repo.commit_all("first");
    assert!(sessions(&repo, &sha).is_empty());
}

#[test]
fn a_commit_without_a_note_yields_no_sessions() {
    isolate_claude_projects();
    let repo = init_repo();
    repo.write("a.txt", "one\n");
    let first = repo.commit_all("first");
    write_note(&repo, &first, &fixture("single-claude-session.note"));

    repo.write("a.txt", "one\ntwo\n");
    let second = repo.commit_all("second");
    assert!(sessions(&repo, &second).is_empty());
    assert_eq!(sessions(&repo, &first).len(), 1);
}

#[test]
fn a_claude_session_becomes_one_event_with_the_mapped_tool_name() {
    isolate_claude_projects();
    let repo = init_repo();
    repo.write("a.txt", "one\n");
    let sha = repo.commit_all("first");
    write_note(&repo, &sha, &fixture("single-claude-session.note"));

    let found = sessions(&repo, &sha);
    assert_eq!(found.len(), 1, "the human entry is not an AI session");
    let s = &found[0];
    assert_eq!(s.tool, "claude-code", "`claude` maps to our name");
    assert_eq!(s.model.as_deref(), Some("claude-fable-5"));
    assert_eq!(s.session_id, "aa2b7669-72a3-4ce8-8fbf-62fd9caee4f5");
    assert!(s.accepted);
    assert_eq!(s.response_excerpt, None, "never recorded in v1");
    assert_eq!(s.prompt, None, "no transcript on disk");
    assert_eq!(s.files.len(), 1);
    assert_eq!(s.files[0].path, "src/daemon.rs");
    assert_eq!(s.files[0].ai_lines, 16);
    assert_eq!(
        s.files[0].ai_lines_modified_by_human, 0,
        "one note cannot show human rework"
    );
}

#[test]
fn copilot_is_renamed_and_a_legacy_prompt_spreads_its_overrides() {
    isolate_claude_projects();
    let repo = init_repo();
    repo.write("a.txt", "one\n");
    let sha = repo.commit_all("first");
    write_note(&repo, &sha, &fixture("mixed-ai-human-legacy.note"));

    let found = sessions(&repo, &sha);
    assert_eq!(found.len(), 2, "one session, one legacy prompt: {found:?}");

    let copilot = &found[0];
    assert_eq!(
        copilot.tool, "copilot",
        "`github-copilot` maps to `copilot`"
    );
    assert_eq!(copilot.model, None);
    assert_eq!(copilot.session_id, "cop-991");
    assert_eq!(
        copilot
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.ai_lines))
            .collect::<Vec<_>>(),
        [("cli/src/gitai.rs", 46), ("docs/spec.md", 12)]
    );
    assert!(copilot
        .files
        .iter()
        .all(|f| f.ai_lines_modified_by_human == 0));

    let legacy = &found[1];
    assert_eq!(legacy.tool, "cursor", "unmapped tools keep their name");
    assert_eq!(legacy.session_id, "cur-7781");
    // 5 overridden lines over 5 + 10 attributed lines, largest remainder first.
    assert_eq!(
        legacy
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.ai_lines, f.ai_lines_modified_by_human))
            .collect::<Vec<_>>(),
        [("README.md", 5, 2), ("docs/spec.md", 10, 3)]
    );
}

#[test]
fn a_session_key_the_note_does_not_describe_is_skipped() {
    isolate_claude_projects();
    let repo = init_repo();
    repo.write("a.txt", "one\n");
    let sha = repo.commit_all("first");
    write_note(&repo, &sha, &fixture("unknown-key.note"));

    let found = sessions(&repo, &sha);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].session_id, "0f0f0f0f-1111-2222-3333-444444444444");
}

#[test]
fn a_legacy_prompt_without_a_sessions_map_still_becomes_an_event() {
    isolate_claude_projects();
    let repo = init_repo();
    repo.write("a.txt", "one\n");
    let sha = repo.commit_all("first");
    write_note(&repo, &sha, &fixture("no-sessions-map.note"));

    let found = sessions(&repo, &sha);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].tool, "windsurf");
    assert_eq!(found[0].files[0].ai_lines, 10);
    assert_eq!(found[0].files[0].ai_lines_modified_by_human, 0);
}

#[test]
fn a_note_that_cannot_be_parsed_is_not_an_error() {
    isolate_claude_projects();
    let repo = init_repo();
    repo.write("a.txt", "one\n");
    let sha = repo.commit_all("first");
    write_note(&repo, &sha, &fixture("malformed-json.note"));

    assert!(
        sessions(&repo, &sha).is_empty(),
        "a broken note must never stand between you and your commit"
    );
}

// ---------------------------------------------------------------------------
// the Claude Code transcript
// ---------------------------------------------------------------------------

#[test]
fn the_project_directory_encoding_matches_claude_code() {
    // Claude Code turns everything that is not a letter or a digit into `-`.
    assert_eq!(
        prompt::encode_project_dir(Path::new("/Users/vanja/Desktop/CODING PROJECTS/POP CLI")),
        "-Users-vanja-Desktop-CODING-PROJECTS-POP-CLI"
    );
    assert_eq!(
        prompt::encode_project_dir(Path::new("/Users/v/p/.claude/worktrees/a+b")),
        "-Users-v-p--claude-worktrees-a-b"
    );
    // A trailing separator, as git2 hands back workdirs, changes nothing.
    assert_eq!(
        prompt::encode_project_dir(Path::new("/private/tmp/")),
        "-private-tmp"
    );
}

#[test]
fn the_first_user_message_of_the_transcript_becomes_the_prompt() {
    let base = TempDir::new().unwrap();
    let workdir = Path::new("/private/tmp/some project");
    write_transcript(
        base.path(),
        workdir,
        "sess-1",
        &[
            r#"{"type":"queue-operation","operation":"add"}"#,
            &serde_json::json!({
                "type": "user",
                "message": {"role": "user", "content": [{"type": "tool_result", "content": "ok"}]}
            })
            .to_string(),
            &user_line("  Add a  reader\n for Git AI notes.  "),
            &user_line("and now something else"),
        ],
    );

    let filter = ExcerptFilter::new(&[]).unwrap();
    assert_eq!(
        prompt::lookup(base.path(), workdir, "sess-1", &filter).as_deref(),
        Some("Add a reader for Git AI notes."),
        "whitespace is collapsed and tool results are skipped"
    );
}

#[test]
fn a_text_block_content_array_is_read_too() {
    let base = TempDir::new().unwrap();
    let workdir = Path::new("/private/tmp/blocks");
    write_transcript(
        base.path(),
        workdir,
        "sess-2",
        &[&serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type": "text", "text": "first block"},
                {"type": "text", "text": "second block"}
            ]}
        })
        .to_string()],
    );

    let filter = ExcerptFilter::new(&[]).unwrap();
    assert_eq!(
        prompt::lookup(base.path(), workdir, "sess-2", &filter).as_deref(),
        Some("first block second block")
    );
}

#[test]
fn a_long_prompt_is_cut_at_five_hundred_characters() {
    let base = TempDir::new().unwrap();
    let workdir = Path::new("/private/tmp/long");
    let long = "abcdefghij".repeat(80);
    write_transcript(base.path(), workdir, "sess-3", &[&user_line(&long)]);

    let filter = ExcerptFilter::new(&[]).unwrap();
    let got = prompt::lookup(base.path(), workdir, "sess-3", &filter).unwrap();
    assert_eq!(got.chars().count(), 500);
    assert!(long.starts_with(&got));
}

#[test]
fn a_prompt_matching_a_redact_pattern_is_dropped() {
    let base = TempDir::new().unwrap();
    let workdir = Path::new("/private/tmp/secret");
    write_transcript(
        base.path(),
        workdir,
        "sess-4",
        &[&user_line("deploy with token sk-live-abc123")],
    );

    let allowed = ExcerptFilter::new(&[]).unwrap();
    assert!(prompt::lookup(base.path(), workdir, "sess-4", &allowed).is_some());

    let redacting = ExcerptFilter::new(&["sk-live-[A-Za-z0-9]+".to_string()]).unwrap();
    assert_eq!(
        prompt::lookup(base.path(), workdir, "sess-4", &redacting),
        None,
        "a matching prompt is dropped whole, not masked"
    );
}

#[test]
fn a_missing_or_empty_transcript_is_simply_no_prompt() {
    let base = TempDir::new().unwrap();
    let workdir = Path::new("/private/tmp/nothing");
    let filter = ExcerptFilter::new(&[]).unwrap();
    assert_eq!(
        prompt::lookup(base.path(), workdir, "sess-5", &filter),
        None
    );

    write_transcript(base.path(), workdir, "sess-6", &["not json at all", ""]);
    assert_eq!(
        prompt::lookup(base.path(), workdir, "sess-6", &filter),
        None
    );

    // A session id is never allowed to walk out of the projects directory.
    assert_eq!(
        prompt::lookup(base.path(), workdir, "../../../etc/passwd", &filter),
        None
    );
}

#[test]
fn a_claude_session_picks_up_its_transcript_through_the_notes_reader() {
    let base = isolate_claude_projects();
    let repo = init_repo();
    repo.write("a.txt", "one\n");
    let sha = repo.commit_all("first");
    write_note(&repo, &sha, &fixture("single-claude-session.note"));

    let git = git2::Repository::open(repo.root()).unwrap();
    let workdir = git.workdir().unwrap().to_path_buf();
    write_transcript(
        base,
        &workdir,
        "aa2b7669-72a3-4ce8-8fbf-62fd9caee4f5",
        &[&user_line("Write the Git AI reader.")],
    );

    let found = sessions(&repo, &sha);
    assert_eq!(found[0].prompt.as_deref(), Some("Write the Git AI reader."));
}

// ---------------------------------------------------------------------------
// through the hook
// ---------------------------------------------------------------------------

#[test]
fn the_hook_records_ai_sessions_when_the_intent_opted_in() {
    isolate_claude_projects();
    plant_device_key();
    let repo = init_repo();
    init_madelog(&repo);
    repo.write("a.txt", "one\n");
    let sha = repo.commit_all("first");
    write_note(&repo, &sha, &fixture("mixed-ai-human-legacy.note"));

    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    let recorded: Vec<AiSessionPayload> = events(&repo)
        .into_iter()
        .filter(|e| e.kind == EventKind::AiSession)
        .map(|e| {
            assert_eq!(e.commit_sha.as_deref(), Some(sha.as_str()));
            serde_json::from_value(e.payload).unwrap()
        })
        .collect();

    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].tool, "copilot");
    assert_eq!(recorded[1].tool, "cursor");
}

#[test]
fn the_hook_records_no_ai_sessions_when_the_intent_opted_out() {
    isolate_claude_projects();
    plant_device_key();
    let repo = init_repo();
    madelog::run_capture(
        repo.root(),
        &[
            "init",
            "--what",
            "a small cli",
            "--for-whom",
            "a studio",
            "--tools",
            "claude-code",
            "--no-record-ai-sessions",
            "--yes",
        ],
    )
    .unwrap();

    repo.write("a.txt", "one\n");
    let sha = repo.commit_all("first");
    write_note(&repo, &sha, &fixture("mixed-ai-human-legacy.note"));

    madelog::run_in(repo.root(), &["hook", "post-commit"]).unwrap();

    assert!(
        !events(&repo).iter().any(|e| e.kind == EventKind::AiSession),
        "opting out means the notes are never read"
    );
    assert!(
        events(&repo).iter().any(|e| e.kind == EventKind::Commit),
        "the commit itself is still recorded"
    );
}
