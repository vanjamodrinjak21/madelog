//! `madelog capture` — what the hook would record for a commit, and why.
//!
//! A dry run unless you say otherwise. It opens the same tool logs the hook
//! opens, uses the same window the hook used for that commit, and prints the
//! sessions and the per-file attribution it would append.
//!
//! This exists because the interesting failure is silence. When a commit has
//! no `ai_session` event the hook has nothing to say — it is not an error for
//! a commit to be written by hand — so the only way to tell "no session ran"
//! from "the session was not found" is to ask.
//!
//! `--record` appends what the dry run shows, for a commit the hook missed.
//! It only ever adds to a commit that has none: a commit with sessions is
//! already answered, and a second opinion about it is not evidence. Nothing
//! is rewritten, because nothing in the chain can be.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};

use crate::capture::{self, CapturedSession};
use crate::commands::hook::Recorder;
use crate::event::{AiSessionPayload, Payload};
use crate::gitrepo::Repo;
use crate::Ctx;

#[derive(Debug, Default, Clone)]
pub struct Options {
    /// The commit to explain. `None` is HEAD.
    pub commit: Option<String>,
    /// Append the events rather than only showing them.
    pub record: bool,
    /// With `record`: every recorded commit that has no sessions yet.
    pub all: bool,
    pub json: bool,
}

pub fn run(ctx: &mut Ctx<'_>, opts: Options) -> Result<()> {
    let repo = Repo::discover(&ctx.cwd)?;
    let root = repo.workdir()?;
    let Some(recorder) = Recorder::open(&repo)? else {
        bail!(
            "there is no madelog log in {} — run `madelog init` first.",
            root.display()
        );
    };
    if opts.record && !recorder.records_ai_sessions() {
        bail!(
            "this repository has AI sessions turned off, so there is nothing to record. \
             `madelog init --record-ai-sessions` turns them on."
        );
    }

    if opts.all {
        return record_all(ctx, &repo, &recorder, opts.json);
    }

    let sha = resolve(&repo, opts.commit.as_deref())?;
    let meta = repo.commit_meta(&sha)?;
    let (since, until) = recorder.window_for(&sha)?;

    let sessions = capture::collect(&repo, recorder.config(), since, until);
    let notes = crate::gitai::sessions_for_commit(repo.inner(), &sha).unwrap_or_default();
    let diff = repo.commit_diff(&sha)?;
    let payloads = capture::attribute(&sessions, &diff, &root, &notes);

    let recorded = if opts.record {
        Some(record_one(ctx, &recorder, &sha, &payloads)?)
    } else {
        None
    };

    if opts.json {
        return print_json(
            ctx, &sha, &meta, since, until, &sessions, &payloads, recorded,
        );
    }
    if recorded.is_some() {
        return Ok(());
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

/// What a recording run did to one commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recorded {
    /// Appended this many `ai_session` events.
    Appended(usize),
    /// The commit already had sessions; nothing was touched.
    AlreadyHad(usize),
    /// There was nothing to record.
    Nothing,
}

impl Recorded {
    fn appended(self) -> usize {
        match self {
            Recorded::Appended(n) => n,
            _ => 0,
        }
    }
}

/// Append the sessions of one commit, unless it already has some.
fn record_one(
    ctx: &mut Ctx<'_>,
    recorder: &Recorder<'_>,
    sha: &str,
    payloads: &[AiSessionPayload],
) -> Result<Recorded> {
    let store = recorder.store();
    if !store.has_commit(sha)? {
        bail!(
            "{} is not in the log, so there is nothing to attach sessions to. \
             `madelog backfill` records it first.",
            short(sha)
        );
    }

    let existing = store
        .all()?
        .iter()
        .filter(|e| {
            e.kind == crate::event::EventKind::AiSession && e.commit_sha.as_deref() == Some(sha)
        })
        .count();
    if existing > 0 {
        ctx.say(&format!(
            "{} already has {}. Nothing was recorded; the chain is append-only and what is \
             in it stays as it is.",
            short(sha),
            count(existing, "AI session")
        ))?;
        return Ok(Recorded::AlreadyHad(existing));
    }

    if payloads.is_empty() {
        ctx.say(&format!(
            "{}: no AI session wrote anything this commit carries. Nothing was recorded.",
            short(sha)
        ))?;
        return Ok(Recorded::Nothing);
    }

    for payload in payloads {
        store.append_payload(Some(sha), &Payload::AiSession(payload.clone()))?;
    }
    ctx.say(&format!(
        "{}  recorded {}",
        short(sha),
        count(payloads.len(), "AI session")
    ))?;
    for payload in payloads {
        let lines: u64 = payload.files.iter().map(|f| f.ai_lines).sum();
        ctx.say(&format!(
            "  {}  {}  {} across {}",
            payload.tool,
            payload.session_id,
            count(lines as usize, "AI line"),
            count(payload.files.len(), "file")
        ))?;
    }
    Ok(Recorded::Appended(payloads.len()))
}

/// Every recorded commit that has no sessions yet, oldest first.
fn record_all(ctx: &mut Ctx<'_>, repo: &Repo, recorder: &Recorder<'_>, json: bool) -> Result<()> {
    let root = repo.workdir()?;
    let store = recorder.store();
    let all = store.recorded_commits()?;

    let mut todo = Vec::new();
    let mut skipped = 0usize;
    for sha in &all {
        if store.has_ai_sessions(sha)? {
            skipped += 1;
        } else {
            todo.push(sha.clone());
        }
    }

    if !json {
        ctx.say(&format!(
            "{} in the log, {} already answered, {} to look at.",
            count(all.len(), "commit"),
            count(skipped, "commit"),
            todo.len()
        ))?;
        ctx.blank()?;
    }

    let mut commits_touched = 0usize;
    let mut events = 0usize;
    let mut done: Vec<serde_json::Value> = Vec::new();
    for sha in &todo {
        let Ok((since, until)) = recorder.window_for(sha) else {
            continue;
        };
        let sessions = capture::collect(repo, recorder.config(), since, until);
        let notes = crate::gitai::sessions_for_commit(repo.inner(), sha).unwrap_or_default();
        let Ok(diff) = repo.commit_diff(sha) else {
            continue;
        };
        let payloads = capture::attribute(&sessions, &diff, &root, &notes);
        if payloads.is_empty() {
            continue;
        }

        let mut quiet: Vec<u8> = Vec::new();
        let outcome = {
            let mut silent = Ctx {
                cwd: ctx.cwd.clone(),
                input: &mut std::io::empty(),
                out: &mut quiet,
            };
            record_one(&mut silent, recorder, sha, &payloads)?
        };
        let n = outcome.appended();
        if n == 0 {
            continue;
        }
        commits_touched += 1;
        events += n;

        if json {
            done.push(serde_json::json!({
                "commit": sha,
                "recorded": n,
                "window": { "since": stamp(since), "until": stamp(until) },
                "sessions": payloads,
            }));
        } else {
            ctx.out.write_all(&quiet)?;
        }
    }

    if json {
        let body = serde_json::to_string_pretty(&serde_json::json!({
            "commits": all.len(),
            "already_had_sessions": skipped,
            "looked_at": todo.len(),
            "commits_recorded": commits_touched,
            "events_recorded": events,
            "recorded": done,
        }))
        .context("rendering the capture as JSON")?;
        return ctx.say(&body);
    }

    ctx.blank()?;
    ctx.say(&format!(
        "recorded {} across {}; {} had nothing to attribute",
        count(events, "AI session"),
        count(commits_touched, "commit"),
        count(todo.len() - commits_touched, "commit")
    ))?;
    if events > 0 {
        ctx.say("the page is older than the log now — `madelog open` renders it again")?;
    }
    Ok(())
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
    recorded: Option<Recorded>,
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
        "recorded": recorded.map(Recorded::appended),
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
