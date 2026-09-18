//! `madelog capture` — what the hook would record for a commit, and why.
//!
//! A dry run, always. It opens the same tool logs the hook opens, uses the
//! same window the hook would have used for that commit, and prints the
//! sessions and the per-file attribution it would append. Nothing is written:
//! no event, no page, no file.
//!
//! This exists because the interesting failure is silence. When a commit has
//! no `ai_session` event the hook has nothing to say — it is not an error for
//! a commit to be written by hand — so the only way to tell "no session ran"
//! from "the session was not found" is to ask.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};

use crate::capture::{self, CapturedSession};
use crate::commands::hook::Recorder;
use crate::event::AiSessionPayload;
use crate::gitrepo::Repo;
use crate::Ctx;

pub fn run(ctx: &mut Ctx<'_>, commit: Option<&str>, json: bool) -> Result<()> {
    let repo = Repo::discover(&ctx.cwd)?;
    let root = repo.workdir()?;
    let Some(recorder) = Recorder::open(&repo)? else {
        bail!(
            "there is no madelog log in {} — run `madelog init` first.",
            root.display()
        );
    };

    let sha = resolve(&repo, commit)?;
    let meta = repo.commit_meta(&sha)?;
    let (since, until) = recorder.window_for(&sha)?;

    let sessions = capture::collect(&repo, recorder.config(), since, until);
    let notes = crate::gitai::sessions_for_commit(repo.inner(), &sha).unwrap_or_default();
    let diff = repo.commit_diff(&sha)?;
    let payloads = capture::attribute(&sessions, &diff, &root, &notes);

    if json {
        return print_json(ctx, &sha, &meta, since, until, &sessions, &payloads);
    }
    print_plain(
        ctx,
        &sha,
        &meta,
        since,
        until,
        &sessions,
        &payloads,
        recorder.records_ai_sessions(),
    )
}

/// A sha, a short sha, a tag, `HEAD~2` — whatever git understands. `None` is
/// `HEAD`.
fn resolve(repo: &Repo, commit: Option<&str>) -> Result<String> {
    let Some(raw) = commit.map(str::trim).filter(|v| !v.is_empty()) else {
        return repo
            .head_sha()?
            .context("this repository has no commits yet");
    };
    let object = repo
        .inner()
        .revparse_single(raw)
        .with_context(|| format!("`{raw}` is not a commit this repository has"))?;
    Ok(object
        .peel_to_commit()
        .with_context(|| format!("`{raw}` is not a commit"))?
        .id()
        .to_string())
}

#[allow(clippy::too_many_arguments)]
fn print_plain(
    ctx: &mut Ctx<'_>,
    sha: &str,
    meta: &crate::gitrepo::CommitMeta,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    sessions: &[CapturedSession],
    payloads: &[AiSessionPayload],
    recording: bool,
) -> Result<()> {
    ctx.say(&format!(
        "commit   {}  {}",
        short(sha),
        first_line(&meta.message)
    ))?;
    ctx.say(&format!("window   {} to {}", stamp(since), stamp(until)))?;
    ctx.say(&format!(
        "found    {} in that window",
        count(sessions.len(), "session")
    ))?;
    if !recording {
        ctx.say(
            "note     this repository has AI sessions turned off, so the hook would record \
             none of this",
        )?;
    }
    ctx.say("dry run  nothing was recorded")?;

    if sessions.is_empty() {
        ctx.blank()?;
        ctx.say("No AI session touched this repository in that window.")?;
        for report in capture::report(Some(&ctx.cwd.clone())) {
            ctx.say(&format!("  {}", report.line()))?;
        }
        return Ok(());
    }

    for session in sessions {
        ctx.blank()?;
        let attributed = payloads
            .iter()
            .find(|p| p.session_id == session.session_id && p.tool == session.tool);
        ctx.say(&format!(
            "{}  {}  {}",
            session.tool,
            session.session_id,
            session.model.as_deref().unwrap_or("(model not recorded)")
        ))?;
        ctx.say(&format!(
            "  ran      {} to {}",
            stamp(session.started),
            stamp(session.ended)
        ))?;
        ctx.say(&format!(
            "  prompt   {}",
            session.prompt.as_deref().unwrap_or("(not recorded)")
        ))?;
        ctx.say(&format!(
            "  wrote    {} in this window",
            count(session.writes.len(), "file")
        ))?;

        match attributed {
            None => {
                ctx.say("  would not be recorded against this commit: nothing it wrote is in it")?
            }
            Some(payload) if payload.files.is_empty() => {
                ctx.say("  would be recorded: the session happened, with nothing to attribute")?
            }
            Some(payload) => {
                ctx.say(&format!(
                    "  would be recorded, accepted = {}",
                    payload.accepted
                ))?;
                for file in &payload.files {
                    ctx.say(&format!(
                        "    {:<48} {} ai, {} reworked",
                        file.path, file.ai_lines, file.ai_lines_modified_by_human
                    ))?;
                }
            }
        }

        // Files it wrote that this commit does not carry, which is the usual
        // reason a session looks missing.
        let elsewhere: Vec<String> = session
            .writes
            .iter()
            .filter_map(|w| capture::relative(&ctx.cwd.clone(), &w.path))
            .filter(|path| diff_has(payloads, &session.session_id, path).is_none())
            .collect();
        if !elsewhere.is_empty() {
            ctx.say(&format!(
                "  also wrote, not in this commit: {}",
                elsewhere.join(", ")
            ))?;
        }
    }
    Ok(())
}

fn diff_has(payloads: &[AiSessionPayload], session_id: &str, path: &str) -> Option<()> {
    payloads
        .iter()
        .find(|p| p.session_id == session_id)?
        .files
        .iter()
        .any(|f| f.path == path)
        .then_some(())
}

#[allow(clippy::too_many_arguments)]
fn print_json(
    ctx: &mut Ctx<'_>,
    sha: &str,
    meta: &crate::gitrepo::CommitMeta,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    sessions: &[CapturedSession],
    payloads: &[AiSessionPayload],
) -> Result<()> {
    let found: Vec<serde_json::Value> = sessions
        .iter()
        .map(|s| {
            serde_json::json!({
                "tool": s.tool,
                "session_id": s.session_id,
                "model": s.model,
                "started": stamp(s.started),
                "ended": stamp(s.ended),
                "prompt": s.prompt,
                "prompt_count": s.prompt_count,
                "title": s.title,
                "writes_recorded": s.writes_recorded,
                "wrote": s.writes.len(),
                "lines_added_hint": s.lines_added_hint,
            })
        })
        .collect();

    let body = serde_json::to_string_pretty(&serde_json::json!({
        "commit": sha,
        "message": first_line(&meta.message),
        "window": { "since": stamp(since), "until": stamp(until) },
        "sessions": found,
        "would_record": payloads,
    }))
    .context("rendering the capture as JSON")?;
    ctx.say(&body)
}

fn stamp(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn short(sha: &str) -> String {
    sha[..7.min(sha.len())].to_string()
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}
