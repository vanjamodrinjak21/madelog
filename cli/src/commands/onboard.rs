//! `pop onboard` — what the install script runs last, and what you can run
//! again any time. Spec §6.1.

use anyhow::Result;

use crate::commands::init;
use crate::gitrepo::Repo;
use crate::Ctx;

/// The tools pop cooperates with, and where to get them.
const TOOLS: &[(&str, &str, &str)] = &[
    (
        "git",
        "pop records your commits.",
        "https://git-scm.com/downloads",
    ),
    (
        "git-ai",
        "Without it, AI sessions stay empty.",
        "https://github.com/oleander/git-ai",
    ),
    (
        "ollama",
        "Without it, weekly summaries fall back to a template.",
        "https://ollama.com",
    ),
];

pub fn run(ctx: &mut Ctx<'_>, project: bool, yes: bool) -> Result<()> {
    explain(ctx)?;
    check_tools(ctx)?;
    ctx.blank()?;

    let repo = match Repo::discover(&ctx.cwd) {
        Ok(repo) => repo,
        Err(_) => return outside_a_repo(ctx),
    };
    let workdir = repo.workdir()?;

    if workdir.join(".pop").join("log.db").exists() {
        ctx.say(&format!("pop is already set up in {}.", workdir.display()))?;
        ctx.say("  pop log       see what is recorded")?;
        ctx.say("  pop verify    recompute the chain")?;
        return Ok(());
    }

    let set_up = if project || yes {
        true
    } else {
        ctx.ask_yes_no("Set up pop in this repo now?", true)?
    };

    if !set_up {
        ctx.say("Nothing changed. Run `pop init` when you are ready.")?;
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

fn explain(ctx: &mut Ctx<'_>) -> Result<()> {
    ctx.say("pop records three things: the commits you make, the AI sessions Git AI attributes to them, and the one-sentence decisions you write with `pop note`.")?;
    ctx.say("All of it stays on this machine, in .pop/log.db, until you run `pop publish`.")?;
    ctx.say("No keystrokes, no screenshots, no files git ignores. `pop export` hands you everything back as JSON.")?;
    ctx.blank()
}

fn check_tools(ctx: &mut Ctx<'_>) -> Result<()> {
    for (name, why, url) in TOOLS {
        let line = match init::which(name) {
            Some(path) => format!("{name:<8} found    {}", path.display()),
            None => format!("{name:<8} missing  {why} Install it from {url}"),
        };
        ctx.say(&line)?;
    }
    Ok(())
}

fn outside_a_repo(ctx: &mut Ctx<'_>) -> Result<()> {
    ctx.say("You are not inside a git repository, so there is nothing to record yet.")?;
    ctx.say("Do this next:")?;
    ctx.say("  git init")?;
    ctx.say("  pop init")?;
    Ok(())
}
