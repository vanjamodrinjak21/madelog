//! `pop init` — four questions, then pop is recording.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::cli::InitArgs;
use crate::config::Config;
use crate::event::{IntentPayload, Payload};
use crate::gitrepo::Repo;
use crate::keys::DeviceKey;
use crate::store::Store;
use crate::Ctx;

/// Marks the lines pop owns inside `.git/hooks/post-commit`.
pub const HOOK_MARKER: &str = "# pop-cli:post-commit";

const HOOK_COMMENT: &str = "# pop-cli:post-commit - records this commit in .pop/log.db";

#[derive(Debug, Default, Clone)]
pub struct Options {
    pub what: Option<String>,
    pub for_whom: Option<String>,
    pub tools: Option<String>,
    pub record_ai_sessions: Option<bool>,
    /// Ask nothing; use what was given and defaults for the rest.
    pub yes: bool,
}

impl Options {
    pub fn from_args(args: &InitArgs) -> Options {
        Options {
            what: args.what.clone(),
            for_whom: args.for_whom.clone(),
            tools: args.tools.clone(),
            record_ai_sessions: if args.no_record_ai_sessions {
                Some(false)
            } else if args.record_ai_sessions {
                Some(true)
            } else {
                None
            },
            yes: args.yes,
        }
    }
}

pub fn run(ctx: &mut Ctx<'_>, opts: Options) -> Result<()> {
    let repo = Repo::discover(&ctx.cwd)?;
    let workdir = repo.workdir()?;
    let pop_dir = workdir.join(".pop");

    let Options {
        what,
        for_whom,
        tools,
        record_ai_sessions,
        yes,
    } = opts;

    let what = answer(ctx, what, yes, "What are you building? ")?;
    let for_whom = answer(ctx, for_whom, yes, "Who is it for? ")?;
    let tools_raw = answer(
        ctx,
        tools,
        yes,
        "Which AI tools do you use? (comma separated) ",
    )?;
    let tools: Vec<String> = tools_raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let record_ai_sessions = match record_ai_sessions {
        Some(v) => v,
        None if yes => true,
        None => ctx.ask_yes_no("Record AI sessions from Git AI?", true)?,
    };

    std::fs::create_dir_all(&pop_dir).with_context(|| format!("creating {}", pop_dir.display()))?;

    write_intent_md(&pop_dir, &what, &for_whom, &tools, record_ai_sessions)?;

    let config_path = pop_dir.join("config.toml");
    let mut config = Config::load(&config_path)?;
    if config.project.name.is_empty() {
        config.project.name = workdir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".to_string());
    }
    config.save(&config_path)?;

    let store = Store::open(&pop_dir.join("log.db"))?;
    let head = repo.head_sha()?;
    store.append_payload(
        head.as_deref(),
        &Payload::Intent(IntentPayload {
            what: what.clone(),
            for_whom: for_whom.clone(),
            tools: tools.clone(),
            record_ai_sessions,
        }),
    )?;

    add_to_git_exclude(&repo.git_dir())?;
    let hook_command = hook_command_for(&workdir, crate::current_exe().as_deref());
    install_post_commit_hook(&repo.git_dir(), &hook_command)?;
    DeviceKey::load_or_create()?;

    ctx.say(&format!("pop is recording in {}", workdir.display()))?;
    ctx.say("  .pop/intent.md      what you are building, in your own words")?;
    ctx.say("  .pop/log.db         the event log, hash chained")?;
    ctx.say("  .pop/config.toml    settings, including privacy.redact")?;
    ctx.say("Your commits are recorded from now on. Nothing leaves this machine.")?;
    ctx.blank()?;
    ctx.say("Next: `pop note \"why you did it that way\"` after a decision.")?;
    if record_ai_sessions && which("git-ai").is_none() {
        ctx.say(
            "Git AI is not on your PATH, so AI sessions stay empty. Install it from \
https://github.com/oleander/git-ai",
        )?;
    }
    Ok(())
}

fn answer(ctx: &mut Ctx<'_>, given: Option<String>, yes: bool, question: &str) -> Result<String> {
    match given {
        Some(v) => Ok(v.trim().to_string()),
        None if yes => Ok(String::new()),
        None => ctx.ask(question, ""),
    }
}

fn write_intent_md(
    pop_dir: &Path,
    what: &str,
    for_whom: &str,
    tools: &[String],
    record_ai_sessions: bool,
) -> Result<()> {
    let tools_line = if tools.is_empty() {
        "none".to_string()
    } else {
        tools.join(", ")
    };
    let body = format!(
        "# Intent\n\n\
         ## What I am building\n\n{what}\n\n\
         ## Who it is for\n\n{for_whom}\n\n\
         ## Tools\n\n{tools_line}\n\n\
         ## AI sessions\n\n{}\n\n\
         Edit this file whenever the answer changes. It is yours; pop only reads it.\n",
        if record_ai_sessions {
            "Recorded from Git AI attribution."
        } else {
            "Not recorded."
        }
    );
    let path = pop_dir.join("intent.md");
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))
}

/// Which `pop` the post-commit hook should call.
///
/// A per-project install puts the binary at `<repo>/.pop/bin/pop` and never
/// touches PATH, so the hook has to name it. Anything else calls `pop`.
pub fn hook_command_for(workdir: &Path, exe: Option<&Path>) -> String {
    let Some(exe) = exe else {
        return "pop".to_string();
    };
    let bin_dir = workdir.join(".pop").join("bin");
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    if canon(exe).starts_with(canon(&bin_dir)) {
        shell_quote(&exe.to_string_lossy())
    } else {
        "pop".to_string()
    }
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

/// Install (or top up) `.git/hooks/post-commit`. An existing hook is kept and
/// our line is appended once, however many times this runs.
pub fn install_post_commit_hook(git_dir: &Path, command: &str) -> Result<()> {
    let hooks = git_dir.join("hooks");
    std::fs::create_dir_all(&hooks).with_context(|| format!("creating {}", hooks.display()))?;
    let path = hooks.join("post-commit");
    let line = format!("{HOOK_COMMENT}\n{command} hook post-commit || true\n");

    let contents = match std::fs::read_to_string(&path) {
        Ok(existing) => {
            if existing.contains(HOOK_MARKER) {
                return Ok(());
            }
            let mut merged = existing;
            if !merged.ends_with('\n') {
                merged.push('\n');
            }
            merged.push('\n');
            merged.push_str(&line);
            merged
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            format!("#!/bin/sh\n{line}")
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };

    std::fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
    make_executable(&path)
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

/// Keep `.pop/` out of the repository without touching the user's `.gitignore`.
pub fn add_to_git_exclude(git_dir: &Path) -> Result<()> {
    let info = git_dir.join("info");
    std::fs::create_dir_all(&info).with_context(|| format!("creating {}", info.display()))?;
    let path = info.join("exclude");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == ".pop/") {
        return Ok(());
    }
    let mut merged = existing;
    if !merged.is_empty() && !merged.ends_with('\n') {
        merged.push('\n');
    }
    merged.push_str(".pop/\n");
    std::fs::write(&path, merged).with_context(|| format!("writing {}", path.display()))
}

/// Find an executable on PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".to_string())
            .split(';')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &extensions {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_hook_gets_a_shebang_and_our_line() {
        let dir = tempfile::TempDir::new().unwrap();
        let git_dir = dir.path().join(".git");
        install_post_commit_hook(&git_dir, "pop").unwrap();
        let hook = std::fs::read_to_string(git_dir.join("hooks/post-commit")).unwrap();
        assert!(hook.starts_with("#!/bin/sh\n"), "{hook}");
        assert!(hook.contains("pop hook post-commit || true"), "{hook}");
    }

    #[test]
    fn an_existing_hook_is_kept_and_topped_up_once() {
        let dir = tempfile::TempDir::new().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(git_dir.join("hooks")).unwrap();
        std::fs::write(git_dir.join("hooks/post-commit"), "#!/bin/bash\necho hi").unwrap();

        install_post_commit_hook(&git_dir, "pop").unwrap();
        install_post_commit_hook(&git_dir, "pop").unwrap();

        let hook = std::fs::read_to_string(git_dir.join("hooks/post-commit")).unwrap();
        assert!(hook.contains("echo hi"), "{hook}");
        assert_eq!(hook.matches(HOOK_MARKER).count(), 1, "{hook}");
    }

    #[test]
    fn the_hook_names_a_per_project_binary() {
        let dir = tempfile::TempDir::new().unwrap();
        let exe = dir.path().join(".pop").join("bin").join("pop");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "binary").unwrap();

        let command = hook_command_for(dir.path(), Some(&exe));
        assert!(command.contains(".pop/bin/pop"), "{command}");
        assert_ne!(command, "pop");
    }

    #[test]
    fn a_binary_anywhere_else_is_called_by_name() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(
            hook_command_for(dir.path(), Some(Path::new("/usr/local/bin/pop"))),
            "pop"
        );
        assert_eq!(hook_command_for(dir.path(), None), "pop");
    }

    #[test]
    fn paths_with_spaces_are_quoted() {
        assert_eq!(shell_quote("/a b/pop"), "'/a b/pop'");
        assert_eq!(shell_quote("/a/pop"), "/a/pop");
    }

    #[test]
    fn the_exclude_entry_is_added_once() {
        let dir = tempfile::TempDir::new().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(git_dir.join("info")).unwrap();
        std::fs::write(git_dir.join("info/exclude"), "# existing\n*.log").unwrap();

        add_to_git_exclude(&git_dir).unwrap();
        add_to_git_exclude(&git_dir).unwrap();

        let body = std::fs::read_to_string(git_dir.join("info/exclude")).unwrap();
        assert!(body.contains("*.log"), "{body}");
        assert_eq!(body.matches(".pop/").count(), 1, "{body}");
    }

    #[test]
    fn which_finds_a_program_that_exists() {
        assert!(which("sh").is_some() || which("cmd").is_some());
        assert!(which("a-program-that-does-not-exist-0000").is_none());
    }
}
