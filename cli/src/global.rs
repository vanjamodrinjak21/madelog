//! What madelog knows about this machine rather than about one repository.
//!
//! Three small things live in `~/.config/madelog`:
//!
//! ```text
//! config.toml   auto_init, and what a repository madelog sets up itself gets
//! hooks/        post-commit, post-merge, post-rewrite for core.hooksPath
//! hinted/       one empty file per repository already told about madelog
//! ```
//!
//! The hooks directory is what makes madelog automatic: git runs the hooks in
//! `core.hooksPath` in *every* repository, so madelog gets a chance to record
//! a commit in a project you have not set up yet. Whatever `core.hooksPath`
//! pointed at before is chained, never replaced.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::keys::config_dir;

/// The hooks madelog installs globally, and what each one is for.
pub const HOOKS: &[(&str, &str)] = &[
    (
        "post-commit",
        "madelog decides what to do in this repository",
    ),
    ("post-merge", "the commit a merge or pull left at HEAD"),
    ("post-rewrite", "the commits a rebase or amend produced"),
];

/// The line that marks madelog's own script.
pub const MARKER: &str = "# madelog-cli:global";

/// Where the global git config is. `$MADELOG_GIT_CONFIG` overrides it, which
/// is how the tests avoid writing to whoever is running them.
pub const GIT_CONFIG_ENV: &str = "MADELOG_GIT_CONFIG";

// ---------------------------------------------------------------------------
// ~/.config/madelog/config.toml
// ---------------------------------------------------------------------------

/// The handful of settings that are about this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GlobalConfig {
    /// Set madelog up by itself in any repository you commit in. Off unless
    /// you said yes at `madelog onboard`.
    pub auto_init: bool,
    /// What `record_ai_sessions` should be in a repository madelog sets up on
    /// its own. You are never asked twice.
    pub record_ai_sessions: bool,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        GlobalConfig {
            auto_init: false,
            record_ai_sessions: true,
        }
    }
}

impl GlobalConfig {
    pub fn path() -> Result<PathBuf> {
        Ok(config_dir()?.join("config.toml"))
    }

    /// Load it, or hand back the defaults. A file that will not parse is not
    /// worth failing a commit over.
    pub fn load() -> GlobalConfig {
        let Ok(path) = GlobalConfig::path() else {
            return GlobalConfig::default();
        };
        GlobalConfig::load_from(&path)
    }

    pub fn load_from(path: &Path) -> GlobalConfig {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| toml::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<PathBuf> {
        let path = GlobalConfig::path()?;
        self.save_to(&path)?;
        Ok(path)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let body = toml::to_string_pretty(self).context("serialising the global config")?;
        let header = "# madelog, on this machine. One repository's settings live in its own \
                      .madelog/config.toml.\n";
        std::fs::write(path, format!("{header}{body}"))
            .with_context(|| format!("writing {}", path.display()))
    }
}

// ---------------------------------------------------------------------------
// the global hooks directory
// ---------------------------------------------------------------------------

pub fn hooks_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("hooks"))
}

pub fn hinted_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("hinted"))
}

/// What happened when the global hooks were turned on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enabled {
    pub dir: PathBuf,
    /// The hooks directory that was there before, now called from ours.
    pub chained: Option<PathBuf>,
    /// `false` when git already pointed at our directory.
    pub changed: bool,
}

/// Install the global hooks and point `core.hooksPath` at them.
///
/// Idempotent, and careful with what it finds: a `core.hooksPath` that is
/// already ours is left alone, and any other one is chained rather than
/// replaced, so hooks somebody set up on purpose keep running.
pub fn enable(command: &str) -> Result<Enabled> {
    let dir = hooks_dir()?;
    let existing = hooks_path()?;
    let chained = match &existing {
        Some(path) if same_dir(path, &dir) => None,
        Some(path) => Some(path.clone()),
        None => None,
    };
    let already_ours = existing.as_ref().is_some_and(|p| same_dir(p, &dir));

    write_hooks(&dir, command, chained.as_deref())?;
    if !already_ours {
        set_hooks_path(&dir)?;
    }
    Ok(Enabled {
        dir,
        chained,
        changed: !already_ours,
    })
}

/// Are the global hooks installed and pointed at?
pub fn is_enabled() -> bool {
    let Ok(dir) = hooks_dir() else {
        return false;
    };
    hooks_path()
        .ok()
        .flatten()
        .is_some_and(|p| same_dir(&p, &dir))
        && HOOKS.iter().all(|(name, _)| dir.join(name).is_file())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

fn write_hooks(dir: &Path, command: &str, chained: Option<&Path>) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for (name, why) in HOOKS {
        let path = dir.join(name);
        std::fs::write(&path, script(name, why, command, chained))
            .with_context(|| format!("writing {}", path.display()))?;
        make_executable(&path)?;
    }
    Ok(())
}

/// One global hook script.
///
/// `post-rewrite` is the only one git speaks to on stdin, so its script reads
/// the input once and hands the same bytes to both madelog and whatever hook
/// was chained. The other two need no such care.
fn script(name: &str, why: &str, command: &str, chained: Option<&Path>) -> String {
    let mut body = format!("#!/bin/sh\n{MARKER}:{name} - {why}\n");
    let chain = chained.map(|dir| shell_quote(&dir.join(name).to_string_lossy()));

    if name == "post-rewrite" {
        body.push_str("madelog_stdin=$(cat)\n");
        body.push_str(&format!(
            "printf '%s' \"$madelog_stdin\" | {command} hook auto {name} || true\n"
        ));
        if let Some(previous) = chain {
            body.push_str(&format!("if [ -x {previous} ]; then\n"));
            body.push_str(&format!(
                "  printf '%s' \"$madelog_stdin\" | {previous} \"$@\"\n"
            ));
            body.push_str("fi\n");
        }
        return body;
    }

    body.push_str(&format!("{command} hook auto {name} || true\n"));
    if let Some(previous) = chain {
        body.push_str(&format!(
            "if [ -x {previous} ]; then\n  {previous} \"$@\"\nfi\n"
        ));
    }
    body
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(path, perms)
        .with_context(|| format!("making {} executable", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// core.hooksPath
// ---------------------------------------------------------------------------

/// The global git config file, created empty if it is not there yet.
pub fn git_config_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(GIT_CONFIG_ENV) {
        if !path.is_empty() {
            let path = PathBuf::from(path);
            ensure_file(&path)?;
            return Ok(path);
        }
    }
    if let Ok(path) = git2::Config::find_global() {
        return Ok(path);
    }
    let home = dirs::home_dir().context("cannot find your home directory")?;
    let path = home.join(".gitconfig");
    ensure_file(&path)?;
    Ok(path)
}

fn ensure_file(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, "").with_context(|| format!("creating {}", path.display()))
}

/// `git config --global core.hooksPath`, if it is set.
pub fn hooks_path() -> Result<Option<PathBuf>> {
    let path = git_config_path()?;
    let config =
        git2::Config::open(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(config
        .get_string("core.hooksPath")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(expand_tilde))
}

pub fn set_hooks_path(dir: &Path) -> Result<()> {
    let path = git_config_path()?;
    let mut config =
        git2::Config::open(&path).with_context(|| format!("opening {}", path.display()))?;
    config
        .set_str("core.hooksPath", &dir.to_string_lossy())
        .with_context(|| format!("setting core.hooksPath in {}", path.display()))
}

fn expand_tilde(value: String) -> PathBuf {
    match value.strip_prefix("~/") {
        Some(rest) => match dirs::home_dir() {
            Some(home) => home.join(rest),
            None => PathBuf::from(value),
        },
        None => PathBuf::from(value),
    }
}

// ---------------------------------------------------------------------------
// the one-time hint
// ---------------------------------------------------------------------------

/// The marker file that says this repository has already been told about
/// madelog.
pub fn hint_marker(repo_root: &Path) -> Result<PathBuf> {
    Ok(hinted_dir()?.join(repo_hash(repo_root)))
}

/// A short, stable name for a repository path. Hashed rather than spelled out
/// so the directory does not turn into a list of everything you work on.
pub fn repo_hash(repo_root: &Path) -> String {
    let canon = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canon.to_string_lossy().as_bytes());
    hex::encode(&hasher.finalize()[..8])
}

/// Should this repository be told about madelog? True once, ever.
pub fn should_hint(repo_root: &Path) -> bool {
    let Ok(dir) = hinted_dir() else {
        return false;
    };
    should_hint_in(&dir, repo_root)
}

fn should_hint_in(dir: &Path, repo_root: &Path) -> bool {
    let marker = dir.join(repo_hash(repo_root));
    if marker.exists() {
        return false;
    }
    let _ = std::fs::create_dir_all(dir);
    std::fs::write(&marker, "").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_global_config_defaults_to_not_touching_your_repositories() {
        let c = GlobalConfig::default();
        assert!(!c.auto_init, "madelog never sets itself up uninvited");
        assert!(c.record_ai_sessions);
    }

    #[test]
    fn the_global_config_round_trips() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let c = GlobalConfig {
            auto_init: true,
            record_ai_sessions: false,
        };
        c.save_to(&path).unwrap();
        assert_eq!(GlobalConfig::load_from(&path), c);

        // A file that is not there, and one that is nonsense, both read as
        // the defaults rather than as an error.
        assert_eq!(
            GlobalConfig::load_from(&dir.path().join("gone.toml")),
            GlobalConfig::default()
        );
        std::fs::write(&path, "this is not toml = = =").unwrap();
        assert_eq!(GlobalConfig::load_from(&path), GlobalConfig::default());
    }

    #[test]
    fn a_hook_script_calls_madelog_and_nothing_else_when_there_was_nothing_else() {
        let body = script("post-commit", "why", "madelog", None);
        assert!(body.starts_with("#!/bin/sh\n"), "{body}");
        assert!(
            body.contains("madelog hook auto post-commit || true"),
            "{body}"
        );
        assert!(!body.contains("if [ -x"), "{body}");
    }

    #[test]
    fn a_hook_script_chains_what_was_there_before() {
        let body = script(
            "post-commit",
            "why",
            "madelog",
            Some(Path::new("/a b/hooks")),
        );
        assert!(body.contains("madelog hook auto post-commit"), "{body}");
        assert!(body.contains("'/a b/hooks/post-commit' \"$@\""), "{body}");
    }

    #[test]
    fn post_rewrite_hands_the_same_stdin_to_both() {
        let body = script("post-rewrite", "why", "madelog", Some(Path::new("/old")));
        assert!(body.contains("madelog_stdin=$(cat)"), "{body}");
        assert_eq!(
            body.matches("printf '%s' \"$madelog_stdin\"").count(),
            2,
            "{body}"
        );
        assert!(body.contains("/old/post-rewrite"), "{body}");
    }

    #[test]
    fn a_repository_is_hinted_at_once() {
        let hinted = tempfile::TempDir::new().unwrap();
        let repo = tempfile::TempDir::new().unwrap();
        assert!(should_hint_in(hinted.path(), repo.path()));
        assert!(!should_hint_in(hinted.path(), repo.path()), "never twice");

        let other = tempfile::TempDir::new().unwrap();
        assert!(
            should_hint_in(hinted.path(), other.path()),
            "a different repo is its own"
        );
        assert_ne!(repo_hash(repo.path()), repo_hash(other.path()));
    }
}
