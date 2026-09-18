//! Shared helpers for the integration tests: throwaway git repos and an
//! isolated device-key directory so tests never touch `~/.config/madelog`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tempfile::TempDir;

static KEY_DIR: OnceLock<TempDir> = OnceLock::new();

/// Point `madelog` at a throwaway config dir for the whole test binary.
///
/// Every test in a binary gets the same directory, so repeatedly setting the
/// variable is a no-op rather than a race.
pub fn isolate_device_key() {
    // Never step on a config dir a test already chose: `isolate_machine`
    // hands out one per test, and overwriting it would undo the isolation.
    if std::env::var_os("MADELOG_CONFIG_DIR").is_some() {
        return;
    }
    let dir = KEY_DIR.get_or_init(|| TempDir::new().expect("temp key dir"));
    std::env::set_var("MADELOG_CONFIG_DIR", dir.path());
}

static PATH_WITH_MADELOG: OnceLock<String> = OnceLock::new();
static PATH_DIR: OnceLock<TempDir> = OnceLock::new();

/// Put a stand-in `madelog` on `PATH` for the whole test binary.
///
/// Checks that ask "is the binary this hook calls actually there?" need a
/// truthful answer, and a test binary is not itself on `PATH`. Idempotent:
/// the composed value is computed once, so calling this from every test
/// cannot grow `PATH` one copy at a time.
pub fn madelog_on_path() {
    let composed = PATH_WITH_MADELOG.get_or_init(|| {
        let dir = PATH_DIR.get_or_init(|| TempDir::new().expect("temp bin dir"));
        let exe = dir.path().join(if cfg!(windows) {
            "madelog.exe"
        } else {
            "madelog"
        });
        std::fs::write(&exe, "#!/bin/sh\nexit 0\n").expect("stand-in madelog");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let existing = std::env::var("PATH").unwrap_or_default();
        let sep = if cfg!(windows) { ";" } else { ":" };
        format!("{}{sep}{existing}", dir.path().display())
    });
    std::env::set_var("PATH", composed);
}

pub struct TestRepo {
    pub dir: TempDir,
}

impl TestRepo {
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    /// Write a file, creating parent directories as needed.
    pub fn write(&self, rel: &str, contents: &str) {
        let p = self.path(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, contents).unwrap();
    }

    /// Stage everything git would stage and commit it. Returns the sha.
    pub fn commit_all(&self, message: &str) -> String {
        self.commit_inner(message, &[])
    }

    /// Stage everything plus explicitly forced paths (bypassing `.gitignore`).
    pub fn commit_all_forcing(&self, message: &str, forced: &[&str]) -> String {
        self.commit_inner(message, forced)
    }

    /// Stage everything and commit it with a committer time of your choosing,
    /// so a test can own a month of history without waiting a month.
    pub fn commit_all_at(&self, message: &str, unix_seconds: i64) -> String {
        self.commit_inner_at(message, &[], Some(unix_seconds))
    }

    fn commit_inner(&self, message: &str, forced: &[&str]) -> String {
        self.commit_inner_at(message, forced, None)
    }

    fn commit_inner_at(&self, message: &str, forced: &[&str], when: Option<i64>) -> String {
        let repo = git2::Repository::open(self.root()).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        for f in forced {
            index.add_path(Path::new(f)).unwrap();
        }
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = match when {
            Some(seconds) => git2::Signature::new(
                "Test Person",
                "test@example.com",
                &git2::Time::new(seconds, 0),
            )
            .unwrap(),
            None => git2::Signature::now("Test Person", "test@example.com").unwrap(),
        };
        let parents: Vec<git2::Commit> = match repo.head() {
            Ok(h) => vec![h.peel_to_commit().unwrap()],
            Err(_) => vec![],
        };
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
            .unwrap();
        oid.to_string()
    }
}

/// A fresh git repository in a temp dir, with identity configured.
pub fn init_repo() -> TestRepo {
    let dir = TempDir::new().expect("temp repo dir");
    let repo = git2::Repository::init(dir.path()).unwrap();
    let mut cfg = repo.config().unwrap();
    cfg.set_str("user.name", "Test Person").unwrap();
    cfg.set_str("user.email", "test@example.com").unwrap();
    drop(cfg);
    drop(repo);
    TestRepo { dir }
}

/// A repo that already has `madelog init` run against it, non-interactively.
///
/// Also points every AI tool adapter at an empty directory. A test that wants
/// a fake `~/.claude` calls [`fake_claude`] *after* this.
pub fn init_madelog(repo: &TestRepo) {
    isolate_device_key();
    isolate_ai_tools();
    madelog::run_capture(
        repo.root(),
        &[
            "init",
            "--what",
            "a small cli that records process",
            "--for-whom",
            "a studio hiring juniors",
            "--tools",
            "claude-code, cursor",
            "--record-ai-sessions",
            "--yes",
        ],
    )
    .expect("madelog init");
}

/// `madelog init` without recording the history this repository already had,
/// for tests that want to do the backfill themselves.
pub fn init_madelog_without_backfill(repo: &TestRepo) {
    isolate_device_key();
    isolate_ai_tools();
    madelog::run_capture(
        repo.root(),
        &[
            "init",
            "--what",
            "a small cli that records process",
            "--for-whom",
            "a studio hiring juniors",
            "--tools",
            "claude-code, cursor",
            "--record-ai-sessions",
            "--no-backfill",
            "--yes",
        ],
    )
    .expect("madelog init");
}

pub fn events(repo: &TestRepo) -> Vec<madelog::event::Event> {
    let store = madelog::store::Store::open(&repo.path(".madelog/log.db")).unwrap();
    store.all().unwrap()
}

pub fn exit_code(err: &anyhow::Error) -> Option<i32> {
    err.downcast_ref::<madelog::ExitError>().map(|e| e.code)
}

// ---------------------------------------------------------------------------
// fake AI tool homes
// ---------------------------------------------------------------------------

use std::sync::{Mutex, MutexGuard};

use chrono::{DateTime, Utc};

/// The home directory of every AI tool madelog can read.
///
/// `MADELOG_CLAUDE_PROJECTS_DIR` is deliberately not here: the Git AI prompt
/// lookup has used it since before capture existed, and its tests set it for
/// their own purposes. Pointing `MADELOG_CLAUDE_HOME` at an empty directory
/// is enough — with no `history.jsonl` there is nothing to enumerate.
pub const TOOL_HOME_VARS: &[&str] = &[
    "MADELOG_CLAUDE_HOME",
    "MADELOG_CODEX_HOME",
    "MADELOG_CURSOR_HOME",
    "MADELOG_COPILOT_HOME",
];

static EMPTY_TOOLS: OnceLock<TempDir> = OnceLock::new();

/// Point every adapter at an empty directory, so a test never reads the AI
/// logs of whoever is running it — and never waits on them either.
///
/// A home a test has already chosen is left alone: environment variables are
/// one per process, and a helper that resets them behind another test's back
/// is a flaky test waiting to happen.
pub fn isolate_ai_tools() {
    let dir = EMPTY_TOOLS.get_or_init(|| TempDir::new().expect("temp tools dir"));
    for var in TOOL_HOME_VARS {
        if std::env::var_os(var).is_none() {
            std::env::set_var(var, dir.path());
        }
    }
}

/// Point every adapter back at an empty directory, whatever was there before.
/// Only for a test that holds [`capture_lock`].
pub fn reset_ai_tools() {
    let dir = EMPTY_TOOLS.get_or_init(|| TempDir::new().expect("temp tools dir"));
    for var in TOOL_HOME_VARS {
        std::env::set_var(var, dir.path());
    }
    std::env::remove_var("MADELOG_CLAUDE_PROJECTS_DIR");
}

static CAPTURE_LOCK: Mutex<()> = Mutex::new(());

/// Hold this for the whole of a test that points the adapters somewhere.
///
/// The homes are environment variables, which are one per process, so tests
/// that set them take turns instead of racing.
pub fn capture_lock() -> MutexGuard<'static, ()> {
    CAPTURE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// One fixture file under `tests/fixtures/capture/`.
pub fn fixture(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/capture")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Substitute this repository's path and one timestamp into a fixture.
fn fill(raw: &str, repo: &TestRepo, when: DateTime<Utc>) -> String {
    let root = repo.root().canonicalize().unwrap();
    let root = root
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    raw.replace("{{REPO_URL}}", &root)
        .replace("{{REPO}}", &root)
        .replace("\"{{TS_MS}}\"", &when.timestamp_millis().to_string())
        .replace(
            "{{TS_ISO}}",
            &when.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        )
}

fn put(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// The session the Claude fixtures describe.
pub const FIXTURE_CLAUDE_SESSION: &str = "aa2b7669-72a3-4ce8-8fbf-62fd9caee4f5";

/// A fake `~/.claude` for `repo`: history, a transcript and a subagent.
///
/// Returns the directory, which the caller keeps alive for as long as the
/// test needs it.
pub fn fake_claude(repo: &TestRepo, when: DateTime<Utc>) -> TempDir {
    let dir = TempDir::new().expect("temp claude home");
    let home = dir.path();
    put(
        &home.join("history.jsonl"),
        &fill(&fixture("claude/history.jsonl"), repo, when),
    );
    // The directory name is a lossy encoding of the working directory, so
    // madelog only ever uses it as a prefilter. Any name will do here.
    let project = home.join("projects").join("-fixture-repo");
    put(
        &project.join(format!("{FIXTURE_CLAUDE_SESSION}.jsonl")),
        &fill(&fixture("claude/transcript.jsonl"), repo, when),
    );
    put(
        &project
            .join(FIXTURE_CLAUDE_SESSION)
            .join("subagents")
            .join("agent-01.jsonl"),
        &fill(&fixture("claude/subagent.jsonl"), repo, when),
    );
    std::env::set_var("MADELOG_CLAUDE_HOME", home);
    std::env::remove_var("MADELOG_CLAUDE_PROJECTS_DIR");
    dir
}

/// What the Claude fixture wrote to `src/parser.rs`, read out of the fixture
/// itself so the test and the transcript can never drift apart.
pub fn fixture_parser_source() -> String {
    tool_use_content("claude/transcript.jsonl", "src/parser.rs")
}

/// What the Codex fixture wrote to `src/codex.rs`.
pub fn fixture_codex_source() -> String {
    let raw = fixture("codex/rollout.jsonl");
    for line in raw.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(changes) = value
            .pointer("/payload/item/changes")
            .and_then(|c| c.as_object())
        else {
            continue;
        };
        for (path, change) in changes {
            if path.ends_with("src/codex.rs") {
                if let Some(content) = change.get("content").and_then(|c| c.as_str()) {
                    return content.to_string();
                }
            }
        }
    }
    panic!("no src/codex.rs in the codex fixture");
}

fn tool_use_content(rel: &str, suffix: &str) -> String {
    let raw = fixture(rel);
    for line in raw.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(result) = value.get("toolUseResult").and_then(|r| r.as_object()) else {
            continue;
        };
        let path = result
            .get("filePath")
            .and_then(|p| p.as_str())
            .unwrap_or("");
        if path.ends_with(suffix) {
            if let Some(content) = result.get("content").and_then(|c| c.as_str()) {
                return content.to_string();
            }
        }
    }
    panic!("no write to {suffix} in {rel}");
}

/// A fake `~/.codex` for `repo`: one rollout here, one somewhere else.
pub fn fake_codex(repo: &TestRepo, when: DateTime<Utc>) -> TempDir {
    let dir = TempDir::new().expect("temp codex home");
    let day = when.format("%Y/%m/%d").to_string();
    let sessions = dir.path().join("sessions").join(day);
    put(
        &sessions.join("rollout-2026-09-18T10-00-00-019a1f7c.jsonl"),
        &fill(&fixture("codex/rollout.jsonl"), repo, when),
    );
    put(
        &sessions.join("rollout-2026-09-18T11-00-00-019a1f7d.jsonl"),
        &fill(&fixture("codex/rollout-elsewhere.jsonl"), repo, when),
    );
    std::env::set_var("MADELOG_CODEX_HOME", dir.path());
    dir
}

/// A fake Cursor `User` directory: one workspace pointing at `repo` and a
/// `state.vscdb` with one `composerHeaders` row, built the way Cursor writes
/// it.
pub fn fake_cursor(repo: &TestRepo, when: DateTime<Utc>) -> TempDir {
    let dir = TempDir::new().expect("temp cursor home");
    let workspace = dir.path().join("workspaceStorage").join("ws0001hash");
    put(
        &workspace.join("workspace.json"),
        &fill(&fixture("cursor/workspace.json"), repo, when),
    );
    // A second workspace that is somebody else's project.
    put(
        &dir.path()
            .join("workspaceStorage")
            .join("ws0002hash")
            .join("workspace.json"),
        "{\"folder\":\"file:///Users/redacted/other-project\"}\n",
    );

    let db = dir.path().join("globalStorage").join("state.vscdb");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE composerHeaders (
            composerId TEXT PRIMARY KEY, workspaceId TEXT, createdAt INTEGER,
            lastUpdatedAt INTEGER, isArchived INTEGER, isSubagent INTEGER,
            recency INTEGER, checkpointAt INTEGER, value TEXT, subagentTypeName TEXT)",
    )
    .unwrap();
    let value = fill(&fixture("cursor/composer-value.json"), repo, when);
    conn.execute(
        "INSERT INTO composerHeaders
         (composerId, workspaceId, createdAt, lastUpdatedAt, isArchived, isSubagent,
          recency, checkpointAt, value, subagentTypeName)
         VALUES (?1, 'ws0001hash', ?2, ?3, 0, 0, 0, 0, ?4, NULL)",
        rusqlite::params![
            "cmp-7781-0000-4000-8000-000000000001",
            when.timestamp_millis(),
            when.timestamp_millis() + 60_000,
            value.trim(),
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO composerHeaders
         (composerId, workspaceId, createdAt, lastUpdatedAt, isArchived, isSubagent,
          recency, checkpointAt, value, subagentTypeName)
         VALUES ('cmp-other', 'ws0002hash', ?1, ?1, 0, 0, 0, 0, '{}', NULL)",
        rusqlite::params![when.timestamp_millis()],
    )
    .unwrap();
    drop(conn);

    std::env::set_var("MADELOG_CURSOR_HOME", dir.path());
    dir
}

/// A fresh `~/.config/madelog` and a fresh global git config, for a test that
/// changes something about the machine rather than about one repository.
///
/// Takes the same lock as [`capture_lock`]: both are about process-wide
/// environment variables, and there is only one process. Hold the guard for
/// the whole test.
pub fn isolate_machine() -> (MutexGuard<'static, ()>, TempDir) {
    let guard = capture_lock();
    let dir = TempDir::new().expect("temp machine dir");
    std::env::set_var("MADELOG_CONFIG_DIR", dir.path());
    std::env::set_var(
        madelog::global::GIT_CONFIG_ENV,
        dir.path().join("gitconfig"),
    );
    reset_ai_tools();
    (guard, dir)
}
