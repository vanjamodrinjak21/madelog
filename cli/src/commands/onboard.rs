//! `madelog onboard` — what the install script runs last, and what you can run
//! again any time. Spec §6.1.

use std::io::IsTerminal;

use anyhow::Result;

use crate::commands::init;
use crate::gitai::GIT_AI_INSTALL;
use crate::gitrepo::Repo;
use crate::Ctx;

/// The brand's terminal banner: the mark in box-drawing characters, printed
/// once before the three explaining lines. `cli/assets/` holds the copy the
/// crate compiles in so `cargo package` works; `design/brand/terminal/` is the
/// source of truth and the two must stay identical.
const BANNER: &str = include_str!("../../assets/banner.txt");
/// The same banner in plain ASCII for terminals that cannot draw boxes.
const BANNER_ASCII: &str = include_str!("../../assets/banner-ascii.txt");

/// The tools madelog cooperates with, and why each one matters.
const TOOLS: &[(&str, &str)] = &[
    ("git", "madelog records your commits."),
    ("git-ai", "Without it, AI sessions stay empty."),
    (
        "ollama",
        "Without it, weekly summaries fall back to a template.",
    ),
];

/// How to get a tool that is missing. Git AI has no download page and no
/// package (`cargo install git-ai` is an unrelated crate), so its hint is the
/// installer command itself.
fn install_hint(name: &str) -> String {
    match name {
        "git" => "Install it from https://git-scm.com/downloads".to_string(),
        "git-ai" => format!("Install it with: {GIT_AI_INSTALL}"),
        _ => "Install it from https://ollama.com".to_string(),
    }
}

pub fn run(ctx: &mut Ctx<'_>, project: bool, yes: bool) -> Result<()> {
    banner(ctx)?;
    explain(ctx)?;
    check_tools(ctx)?;
    ctx.blank()?;

    let repo = match Repo::discover(&ctx.cwd) {
        Ok(repo) => repo,
        Err(_) => return outside_a_repo(ctx),
    };
    let workdir = repo.workdir()?;

    if workdir.join(".madelog").join("log.db").exists() {
        ctx.say(&format!(
            "madelog is already set up in {}.",
            workdir.display()
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
    ctx.say("madelog records three things: the commits you make, the AI sessions Git AI attributes to them, and the one-sentence decisions you write with `madelog note`.")?;
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

fn outside_a_repo(ctx: &mut Ctx<'_>) -> Result<()> {
    ctx.say("You are not inside a git repository, so there is nothing to record yet.")?;
    ctx.say("Do this next:")?;
    ctx.say("  git init")?;
    ctx.say("  madelog init")?;
    Ok(())
}
