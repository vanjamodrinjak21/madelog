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
pub fn init_madelog(repo: &TestRepo) {
    isolate_device_key();
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

pub fn events(repo: &TestRepo) -> Vec<madelog::event::Event> {
    let store = madelog::store::Store::open(&repo.path(".madelog/log.db")).unwrap();
    store.all().unwrap()
}

pub fn exit_code(err: &anyhow::Error) -> Option<i32> {
    err.downcast_ref::<madelog::ExitError>().map(|e| e.code)
}
