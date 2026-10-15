//! `madelog init` — four questions, then madelog is recording.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::cli::InitArgs;
use crate::config::Config;
use crate::event::{IntentPayload, Payload};
use crate::gitrepo::Repo;
use crate::keys::DeviceKey;
use crate::store::Store;
use crate::Ctx;

/// Marks the lines madelog owns inside `.git/hooks/post-commit`.
pub const HOOK_MARKER: &str = "# madelog-cli:post-commit";

/// Every git hook madelog installs, and what each one is for.
///
/// `post-commit` is the one that matters; the other two exist because a rebase
/// or a pull moves HEAD without ever running `post-commit`, and a commit that
/// no hook saw is a hole in the page.
pub const HOOKS: &[(&str, &str)] = &[
    ("post-commit", "records this commit in .madelog/log.db"),
    (
        "post-merge",
        "records the commit a merge or pull left at HEAD",
    ),
    (
        "post-rewrite",
        "records the commits a rebase or amend produced",
    ),
];

/// The line that marks madelog's own lines inside `.git/hooks/<name>`.
pub fn hook_marker(name: &str) -> String {
    format!("# madelog-cli:{name}")
}

#[derive(Debug, Clone)]
pub struct Options {
    pub what: Option<String>,
    pub for_whom: Option<String>,
    pub tools: Option<String>,
    pub record_ai_sessions: Option<bool>,
    /// Ask nothing; use what was given and defaults for the rest.
    pub yes: bool,
    /// Record the commits this repository already had. On by default: a page
    /// that starts on the day you installed madelog says the project took a
    /// day, which is the one thing it must not say.
    pub backfill: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            what: None,
            for_whom: None,
            tools: None,
            record_ai_sessions: None,
            yes: false,
            backfill: true,
        }
    }
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
            backfill: !args.no_backfill,
        }
    }
}

pub fn run(ctx: &mut Ctx<'_>, opts: Options) -> Result<()> {
    let repo = Repo::discover(&ctx.cwd)?;
    let workdir = repo.workdir()?;
    let madelog_dir = workdir.join(".madelog");

    let Options {
        what,
        for_whom,
        tools,
        record_ai_sessions,
        yes,
        backfill,
    } = opts;

    // What this repository already recorded, if anything. A second `init` is a
    // top-up, not a fresh start: it must not quietly replace the intent the
    // page and `madelog export` read.
    let previous = latest_intent(&madelog_dir)?;
    let answers_given = what.is_some() || for_whom.is_some() || tools.is_some();

    std::fs::create_dir_all(&madelog_dir)
        .with_context(|| format!("creating {}", madelog_dir.display()))?;
    let config_path = madelog_dir.join("config.toml");
    let mut config = Config::load(&config_path)?;
    if config.project.name.is_empty() {
        config.project.name = workdir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".to_string());
    }
    config.save(&config_path)?;

    // Everything below this line is idempotent and runs on every `init`.
    add_to_git_exclude(&repo.git_dir())?;
    let hook_command = hook_command_for(&workdir, crate::current_exe().as_deref());
    install_hooks(&repo.git_dir(), &hook_command)?;
    DeviceKey::load_or_create()?;
    // Opening the store creates the log if this is the first run.
    let store = Store::open(&madelog_dir.join("log.db"))?;

    let intent = match &previous {
        // Already recording, and nothing new was said: leave the log and
        // intent.md exactly as they are.
        Some(_) if !answers_given => {
            ctx.say(&format!(
                "madelog is already recording in {}",
                workdir.display()
            ))?;
            ctx.say("Nothing changed. Pass --what, --for-whom or --tools to record a new intent.")?;
            return Ok(());
        }
        // Already recording, and something new was said: a new intent event
        // with the answers merged over the last one.
        Some(previous) => IntentPayload {
            what: given(what).unwrap_or_else(|| previous.what.clone()),
            for_whom: given(for_whom).unwrap_or_else(|| previous.for_whom.clone()),
            tools: tools
                .as_deref()
                .map(tool_list)
                .unwrap_or_else(|| previous.tools.clone()),
            record_ai_sessions: record_ai_sessions.unwrap_or(previous.record_ai_sessions),
        },
        // The first `init`: ask for whatever was not given.
        None => {
            let what = answer(ctx, what, yes, "What are you building? ")?;
            let for_whom = answer(ctx, for_whom, yes, "Who is it for? ")?;
            let tools_raw = answer(
                ctx,
                tools,
                yes,
                "Which AI tools do you use? (comma separated) ",
            )?;
            IntentPayload {
                what,
                for_whom,
                tools: tool_list(&tools_raw),
                record_ai_sessions: match record_ai_sessions {
                    Some(v) => v,
                    None if yes => true,
                    None => ctx.ask_yes_no("Record the AI sessions behind your commits?", true)?,
                },
            }
        }
    };

    write_intent_md(&madelog_dir, &intent)?;
    if is_vague_headline(&intent.what) {
        ctx.say(&format!(
            "note: \"{}\" will be the headline recruiters read first. A sentence like \"A guest photo wall for live events, built as a PWA\" reads better; run `madelog init --what \"...\"` to replace it.",
            intent.what
        ))?;
    }
    store.append_payload(
        repo.head_sha()?.as_deref(),
        &Payload::Intent(intent.clone()),
    )?;

    if previous.is_some() {
        ctx.say(&format!("recorded a new intent in {}", workdir.display()))?;
        ctx.say("  .madelog/intent.md      rewritten with the merged answers")?;
        ctx.say("The old intent stays in the chain; the page reads the newest one.")?;
    } else {
        ctx.say(&format!("madelog is recording in {}", workdir.display()))?;
        ctx.say("  .madelog/intent.md      what you are building, in your own words")?;
        ctx.say("  .madelog/log.db         the event log, hash chained")?;
        ctx.say("  .madelog/config.toml    settings, including privacy.redact")?;
        ctx.say("Your commits are recorded from now on. Nothing leaves this machine.")?;
    }

    if backfill {
        ctx.blank()?;
        backfill_history(ctx)?;
    }

    if previous.is_none() {
        ctx.blank()?;
        ctx.say("Next: `madelog note \"why you did it that way\"` after a decision.")?;
    }
    Ok(())
}

/// Record whatever history this repository already had, without asking.
///
/// A failure here is not a failed `init`: madelog is set up either way, and
/// `madelog backfill` can be run again by hand.
fn backfill_history(ctx: &mut Ctx<'_>) -> Result<()> {
    match crate::commands::backfill::run(
        ctx,
        crate::commands::backfill::Options {
            since: None,
            yes: true,
        },
    ) {
        Ok(()) => Ok(()),
        Err(err) => {
            ctx.say(&format!(
                "could not record the existing history ({err:#}) — run `madelog backfill` later"
            ))?;
            Ok(())
        }
    }
}

/// The newest intent this repository recorded, if it has a log at all.
fn latest_intent(madelog_dir: &Path) -> Result<Option<IntentPayload>> {
    let db = madelog_dir.join("log.db");
    if !db.exists() {
        return Ok(None);
    }
    let found = Store::open(&db)?.last_of_kind(crate::event::EventKind::Intent)?;
    Ok(found.and_then(|e| e.payload_as::<IntentPayload>().ok()))
}

/// A headline too thin to read well: under four words or twenty characters.
/// Only ever a warning; the text is recorded as given.
pub fn is_vague_headline(what: &str) -> bool {
    let what = what.trim();
    what.split_whitespace().count() < 4 || what.chars().count() < 20
}

/// A flag that was given, trimmed. `--what ""` counts as an answer: it clears
/// the line rather than carrying the old one over.
fn given(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_string())
}

fn tool_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn answer(ctx: &mut Ctx<'_>, given: Option<String>, yes: bool, question: &str) -> Result<String> {
    match given {
        Some(v) => Ok(v.trim().to_string()),
        None if yes => Ok(String::new()),
        None => ctx.ask(question, ""),
    }
}

fn write_intent_md(madelog_dir: &Path, intent: &IntentPayload) -> Result<()> {
    let IntentPayload {
        what,
        for_whom,
        tools,
        record_ai_sessions,
    } = intent;
    let record_ai_sessions = *record_ai_sessions;
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
         Edit this file whenever the answer changes. It is yours; madelog only reads it.\n",
        if record_ai_sessions {
            "Recorded from the local logs of the AI tools on this machine."
        } else {
            "Not recorded."
        }
    );
    let path = madelog_dir.join("intent.md");
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))
}

/// Which `madelog` the post-commit hook should call.
///
/// A per-project install puts the binary at `<repo>/.madelog/bin/madelog` and never
/// touches PATH, so the hook has to name it. Anything else calls `madelog`.
pub fn hook_command_for(workdir: &Path, exe: Option<&Path>) -> String {
    let Some(exe) = exe else {
        return "madelog".to_string();
    };
    let bin_dir = workdir.join(".madelog").join("bin");
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    if canon(exe).starts_with(canon(&bin_dir)) {
        shell_quote(&exe.to_string_lossy())
    } else {
        "madelog".to_string()
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

/// Install (or top up) every hook in [`HOOKS`].
pub fn install_hooks(git_dir: &Path, command: &str) -> Result<()> {
    for (name, _) in HOOKS {
        install_hook(git_dir, name, command)?;
    }
    Ok(())
}

/// Install (or top up) `.git/hooks/post-commit`. Kept as its own name because
/// it is the hook everything else is described against.
pub fn install_post_commit_hook(git_dir: &Path, command: &str) -> Result<()> {
    install_hook(git_dir, "post-commit", command)
}

/// Is madelog's line already in `.git/hooks/<name>`?
pub fn hook_is_installed(git_dir: &Path, name: &str) -> bool {
    std::fs::read_to_string(git_dir.join("hooks").join(name))
        .map(|body| body.contains(&hook_marker(name)))
        .unwrap_or(false)
}

/// The command madelog's line in `.git/hooks/<name>` calls, if it has one.
pub fn installed_hook_command(git_dir: &Path, name: &str) -> Option<String> {
    let body = std::fs::read_to_string(git_dir.join("hooks").join(name)).ok()?;
    let marker = hook_marker(name);
    let mut lines = body.lines();
    lines.find(|l| l.trim_start().starts_with(&marker))?;
    let call = lines.next()?.trim();
    let call = call.strip_suffix("|| true").unwrap_or(call).trim();
    let call = call
        .strip_suffix(&format!("hook {name}"))
        .unwrap_or(call)
        .trim();
    Some(unquote(call))
}

/// Undo [`shell_quote`] for the one form it produces.
fn unquote(value: &str) -> String {
    match value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        Some(inner) => inner.replace(r"'\''", "'"),
        None => value.to_string(),
    }
}

/// Install (or top up) one hook. An existing hook is kept and our line is
/// appended once, however many times this runs.
pub fn install_hook(git_dir: &Path, name: &str, command: &str) -> Result<()> {
    let why = HOOKS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, why)| *why)
        .unwrap_or("records commits in .madelog/log.db");
    let hooks = git_dir.join("hooks");
    std::fs::create_dir_all(&hooks).with_context(|| format!("creating {}", hooks.display()))?;
    let path = hooks.join(name);
    let marker = hook_marker(name);
    let line = format!("{marker} - {why}\n{command} hook {name} || true\n");

    let contents = match std::fs::read_to_string(&path) {
        Ok(existing) => {
            if existing.contains(&marker) {
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

/// Keep `.madelog/` out of the repository without touching the user's `.gitignore`.
pub fn add_to_git_exclude(git_dir: &Path) -> Result<()> {
    let info = git_dir.join("info");
    std::fs::create_dir_all(&info).with_context(|| format!("creating {}", info.display()))?;
    let path = info.join("exclude");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == ".madelog/") {
        return Ok(());
    }
    let mut merged = existing;
    if !merged.is_empty() && !merged.ends_with('\n') {
        merged.push('\n');
    }
    merged.push_str(".madelog/\n");
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
    fn short_headlines_are_vague() {
        assert!(is_vague_headline("pwa app"));
        assert!(is_vague_headline("Pwa app"));
        assert!(is_vague_headline(""));
        assert!(is_vague_headline("a b c d"));
        assert!(!is_vague_headline("A guest photo wall for live events"));
    }

    #[test]
    fn a_new_hook_gets_a_shebang_and_our_line() {
        let dir = tempfile::TempDir::new().unwrap();
        let git_dir = dir.path().join(".git");
        install_post_commit_hook(&git_dir, "madelog").unwrap();
        let hook = std::fs::read_to_string(git_dir.join("hooks/post-commit")).unwrap();
        assert!(hook.starts_with("#!/bin/sh\n"), "{hook}");
        assert!(hook.contains("madelog hook post-commit || true"), "{hook}");
    }

    #[test]
    fn an_existing_hook_is_kept_and_topped_up_once() {
        let dir = tempfile::TempDir::new().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(git_dir.join("hooks")).unwrap();
        std::fs::write(git_dir.join("hooks/post-commit"), "#!/bin/bash\necho hi").unwrap();

        install_post_commit_hook(&git_dir, "madelog").unwrap();
        install_post_commit_hook(&git_dir, "madelog").unwrap();

        let hook = std::fs::read_to_string(git_dir.join("hooks/post-commit")).unwrap();
        assert!(hook.contains("echo hi"), "{hook}");
        assert_eq!(hook.matches(HOOK_MARKER).count(), 1, "{hook}");
    }

    #[test]
    fn all_three_hooks_are_installed_and_call_their_own_subcommand() {
        let dir = tempfile::TempDir::new().unwrap();
        let git_dir = dir.path().join(".git");
        install_hooks(&git_dir, "madelog").unwrap();
        install_hooks(&git_dir, "madelog").unwrap();

        for (name, _) in HOOKS {
            let body = std::fs::read_to_string(git_dir.join("hooks").join(name)).unwrap();
            assert!(
                body.contains(&format!("madelog hook {name} || true")),
                "{body}"
            );
            assert_eq!(body.matches(&hook_marker(name)).count(), 1, "{body}");
            assert!(hook_is_installed(&git_dir, name), "{name}");
            assert_eq!(
                installed_hook_command(&git_dir, name).as_deref(),
                Some("madelog"),
                "{name}"
            );
        }
    }

    #[test]
    fn an_uninstalled_hook_names_no_command() {
        let dir = tempfile::TempDir::new().unwrap();
        let git_dir = dir.path().join(".git");
        assert!(!hook_is_installed(&git_dir, "post-merge"));
        assert_eq!(installed_hook_command(&git_dir, "post-merge"), None);

        std::fs::create_dir_all(git_dir.join("hooks")).unwrap();
        std::fs::write(git_dir.join("hooks/post-merge"), "#!/bin/sh\necho hi\n").unwrap();
        assert!(!hook_is_installed(&git_dir, "post-merge"));
        assert_eq!(installed_hook_command(&git_dir, "post-merge"), None);
    }

    #[test]
    fn a_quoted_hook_command_reads_back_as_the_path_it_names() {
        let dir = tempfile::TempDir::new().unwrap();
        let git_dir = dir.path().join(".git");
        install_hook(&git_dir, "post-commit", "'/a b/.madelog/bin/madelog'").unwrap();
        assert_eq!(
            installed_hook_command(&git_dir, "post-commit").as_deref(),
            Some("/a b/.madelog/bin/madelog")
        );
    }

    #[test]
    fn the_hook_names_a_per_project_binary() {
        let dir = tempfile::TempDir::new().unwrap();
        let exe = dir.path().join(".madelog").join("bin").join("madelog");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "binary").unwrap();

        let command = hook_command_for(dir.path(), Some(&exe));
        assert!(command.contains(".madelog/bin/madelog"), "{command}");
        assert_ne!(command, "madelog");
    }

    #[test]
    fn a_binary_anywhere_else_is_called_by_name() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(
            hook_command_for(dir.path(), Some(Path::new("/usr/local/bin/madelog"))),
            "madelog"
        );
        assert_eq!(hook_command_for(dir.path(), None), "madelog");
    }

    #[test]
    fn paths_with_spaces_are_quoted() {
        assert_eq!(shell_quote("/a b/madelog"), "'/a b/madelog'");
        assert_eq!(shell_quote("/a/madelog"), "/a/madelog");
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
        assert_eq!(body.matches(".madelog/").count(), 1, "{body}");
    }

    #[test]
    fn which_finds_a_program_that_exists() {
        assert!(which("sh").is_some() || which("cmd").is_some());
        assert!(which("a-program-that-does-not-exist-0000").is_none());
    }
}
