//! `madelog onboard` — what the install script runs last, and what you can run
//! again any time. Spec §6.1.

use std::io::IsTerminal;
use std::path::Path;

use anyhow::Result;

use crate::capture;
use crate::commands::{hook, init};
use crate::gitrepo::Repo;
use crate::global::{self, GlobalConfig};
use crate::Ctx;

/// The brand's terminal banner: the mark in box-drawing characters, printed
/// once before the three explaining lines. `cli/assets/` holds the copy the
/// crate compiles in so `cargo package` works; `design/brand/terminal/` is the
/// source of truth and the two must stay identical.
const BANNER: &str = include_str!("../../assets/banner.txt");
/// The same banner in plain ASCII for terminals that cannot draw boxes.
const BANNER_ASCII: &str = include_str!("../../assets/banner-ascii.txt");

/// The programs madelog cooperates with, and why each one matters.
///
/// The AI tools are not in this list: madelog reads their logs rather than
/// running them, so what matters is whether they left anything behind, not
/// whether they are on PATH. [`capture::report`] answers that.
const TOOLS: &[(&str, &str)] = &[
    ("git", "madelog records your commits."),
    (
        "ollama",
        "Without it, weekly summaries fall back to a template.",
    ),
];

fn install_hint(name: &str) -> String {
    match name {
        "git" => "Install it from https://git-scm.com/downloads".to_string(),
        _ => "Install it from https://ollama.com".to_string(),
    }
}

pub fn run(ctx: &mut Ctx<'_>, project: bool, yes: bool) -> Result<()> {
    banner(ctx)?;
    explain(ctx)?;
    check_tools(ctx)?;

    let repo = Repo::discover(&ctx.cwd).ok();
    let workdir = repo.as_ref().and_then(|r| r.workdir().ok());

    ctx.blank()?;
    ai_tools(ctx, workdir.as_deref())?;
    ctx.blank()?;

    offer_global_hooks(ctx, project, yes)?;
    claude_skill(ctx)?;

    let Some(workdir) = workdir else {
        return outside_a_repo(ctx);
    };

    ctx.blank()?;
    if workdir.join(".madelog").join("log.db").exists() {
        ctx.say(&format!(
            "madelog is already set up in {}.",
            workdir.display()
        ))?;
        ctx.say(&format!(
            "  AI sessions   {}",
            hook::ai_session_state(&workdir).1
        ))?;
        ctx.say("  madelog log       see what is recorded")?;
        ctx.say("  madelog verify    recompute the chain")?;
        return Ok(());
    }

    let set_up = if project || yes {
        true
    } else {
        ctx.ask_yes_no("Set up madelog in this repo now?", true)?
    };

    if !set_up {
        ctx.say("Nothing changed. Run `madelog init` when you are ready.")?;
        return Ok(());
    }

    ctx.blank()?;
    init::run(
        ctx,
        init::Options {
            // A per-project install already answered for you; an interactive
            // yes should still get the three questions.
            yes: project || yes,
            ..Default::default()
        },
    )
}

/// Print the banner exactly as stored, one colour, no padding. The box-drawing
/// version needs a real terminal that speaks UTF-8; everything else gets ASCII.
fn banner(ctx: &mut Ctx<'_>) -> Result<()> {
    let text = if box_drawing_is_safe() {
        BANNER
    } else {
        BANNER_ASCII
    };
    ctx.out
        .write_all(text.as_bytes())
        .map_err(anyhow::Error::from)?;
    ctx.blank()
}

fn box_drawing_is_safe() -> bool {
    if !std::io::stdout().is_terminal() {
        return false;
    }
    if std::env::var("TERM").is_ok_and(|t| t == "dumb") {
        return false;
    }
    if cfg!(windows) {
        // Windows Terminal renders the glyphs; the legacy console may not.
        return std::env::var_os("WT_SESSION").is_some();
    }
    // A locale that does not name UTF-8 may render the glyphs as garbage.
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .is_some_and(|v| {
            let v = v.to_ascii_lowercase();
            v.contains("utf-8") || v.contains("utf8")
        })
}

fn explain(ctx: &mut Ctx<'_>) -> Result<()> {
    ctx.say("madelog records three things: the commits you make, the AI sessions that produced them, and the one-sentence decisions you write with `madelog note`.")?;
    ctx.say(
        "All of it stays on this machine, in .madelog/log.db, until you run `madelog publish`.",
    )?;
    ctx.say("No keystrokes, no screenshots, no files git ignores. `madelog export` hands you everything back as JSON.")?;
    ctx.blank()
}

fn check_tools(ctx: &mut Ctx<'_>) -> Result<()> {
    for (name, why) in TOOLS {
        let line = match init::which(name) {
            Some(path) => format!("{name:<8} found    {}", path.display()),
            None => format!("{name:<8} missing  {why} {}", install_hint(name)),
        };
        ctx.say(&line)?;
    }
    Ok(())
}

/// Which AI tools left something on this machine that madelog can read, and
/// how much of it is about this repository.
fn ai_tools(ctx: &mut Ctx<'_>, repo_root: Option<&Path>) -> Result<()> {
    ctx.say("AI tools madelog can read:")?;
    for report in capture::report(repo_root) {
        ctx.say(&format!("  {}", report.line()))?;
    }
    if let Some(root) = repo_root {
        ctx.say(&format!(
            "  AI sessions: {}",
            hook::ai_session_state(root).1
        ))?;
    }
    Ok(())
}

/// Offer the global hook, which is what makes madelog work in repositories
/// you have not set up yet.
fn offer_global_hooks(ctx: &mut Ctx<'_>, project: bool, yes: bool) -> Result<()> {
    if global::is_enabled() {
        let config = GlobalConfig::load();
        ctx.say(&format!(
            "Every repository: on. New repositories are {}.",
            if config.auto_init {
                "set up automatically"
            } else {
                "told once that madelog exists"
            }
        ))?;
        return Ok(());
    }

    // A per-project install is about one repository on purpose, and `--yes`
    // must not silently take over a machine's git configuration.
    if project || yes {
        return Ok(());
    }
    if !ctx.ask_yes_no("Record every repository you commit in?", false)? {
        ctx.say("Only the repositories you run `madelog init` in, then.")?;
        return Ok(());
    }

    let auto_init = ctx.ask_yes_no("Set madelog up in a new repository without asking?", true)?;
    let record_ai_sessions =
        ctx.ask_yes_no("Record the AI sessions behind those commits?", true)?;

    let command = init::hook_command_for(&ctx.cwd, crate::current_exe().as_deref());
    let enabled = global::enable(&command)?;
    GlobalConfig {
        auto_init,
        record_ai_sessions,
    }
    .save()?;

    ctx.say(&format!(
        "git will run {} in every repository from now on.",
        enabled.dir.display()
    ))?;
    if let Some(previous) = &enabled.chained {
        ctx.say(&format!(
            "  the hooks you already had in {} still run, right after madelog's",
            previous.display()
        ))?;
    }
    ctx.say(&if auto_init {
        "  a repository with no .madelog/ is set up the first time you commit in it".to_string()
    } else {
        "  a repository with no .madelog/ is told once that `madelog init` exists".to_string()
    })?;
    Ok(())
}

/// Install the Claude Code skill when Claude Code is here. It only teaches
/// Claude to record decisions and to leave publishing to you, so it needs no
/// question; a failure to write it never stops onboarding.
fn claude_skill(ctx: &mut Ctx<'_>) -> Result<()> {
    match crate::skill::install() {
        Ok(crate::skill::Installed::Written(path)) => ctx.say(&format!(
            "Claude Code skill installed at {}: Claude now records your decisions with `madelog note`.",
            path.display()
        )),
        Ok(crate::skill::Installed::Current(_)) => {
            ctx.say("Claude Code skill: up to date.")
        }
        Ok(crate::skill::Installed::NoClaude) => Ok(()),
        Err(e) => ctx.say(&format!("Claude Code skill not installed: {e:#}")),
    }
}

fn outside_a_repo(ctx: &mut Ctx<'_>) -> Result<()> {
    ctx.blank()?;
    ctx.say("You are not inside a git repository, so there is nothing to record yet.")?;
    ctx.say("Do this next:")?;
    ctx.say("  git init")?;
    ctx.say("  madelog init")?;
    Ok(())
}
