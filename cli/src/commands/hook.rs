//! `madelog hook post-commit` — the only thing that runs without you asking.
//!
//! It must never fail a commit. Every error is written to stderr and the
//! command still succeeds.

use anyhow::{Context, Result};

use crate::config::Config;
use crate::event::{CommitPayload, EventKind, IntentPayload, Payload};
use crate::gitrepo::{ExcerptFilter, Repo};
use crate::store::Store;
use crate::Ctx;

pub fn post_commit(ctx: &mut Ctx<'_>) -> Result<()> {
    if let Err(err) = record(ctx) {
        // A broken hook must not stand between you and your commit.
        eprintln!("madelog: could not record this commit: {err:#}");
    }
    Ok(())
}

fn record(ctx: &mut Ctx<'_>) -> Result<()> {
    let repo = Repo::discover(&ctx.cwd)?;
    let madelog_dir = repo.workdir()?.join(".madelog");
    let db = madelog_dir.join("log.db");
    if !db.exists() {
        // madelog is not set up here. Not an error: the hook may outlive `.madelog`.
        return Ok(());
    }

    let Some(sha) = repo.head_sha()? else {
        return Ok(());
    };
    let store = Store::open(&db)?;
    if store.has_commit(&sha)? {
        return Ok(());
    }

    let config = Config::load(&madelog_dir.join("config.toml"))?;
    let filter = ExcerptFilter::new(&config.privacy.redact)?;

    let meta = repo.commit_meta(&sha)?;
    let files = repo.file_changes(&sha, &filter)?;
    let ts = crate::event::now_ts();
    let secs_since_prev = seconds_since_previous_commit(&store, &ts)?;

    store.append_at(
        ts,
        EventKind::Commit,
        Some(&sha),
        Payload::Commit(CommitPayload {
            sha: meta.sha.clone(),
            message: meta.message.clone(),
            author: meta.author.clone(),
            parents: meta.parents.clone(),
            files,
            secs_since_prev,
        })
        .to_value(),
    )?;

    if records_ai_sessions(&store)? {
        // Task B fills this in; until then it is always empty.
        match crate::gitai::sessions_for_commit(repo.inner(), &sha) {
            Ok(sessions) => {
                for session in sessions {
                    store.append_payload(Some(&sha), &Payload::AiSession(session))?;
                }
            }
            Err(err) => eprintln!("madelog: could not read Git AI notes for {sha}: {err:#}"),
        }
    }

    Ok(())
}

/// Wall-clock seconds since the previously recorded commit, or `None` for the
/// first one.
fn seconds_since_previous_commit(store: &Store, now: &str) -> Result<Option<i64>> {
    let Some(previous) = store.last_of_kind(EventKind::Commit)? else {
        return Ok(None);
    };
    let then = chrono::DateTime::parse_from_rfc3339(&previous.ts)
        .with_context(|| format!("event {} has an unreadable timestamp", previous.seq))?;
    let now = chrono::DateTime::parse_from_rfc3339(now).context("bad timestamp")?;
    Ok(Some((now - then).num_seconds().max(0)))
}

fn records_ai_sessions(store: &Store) -> Result<bool> {
    let Some(intent) = store.last_of_kind(EventKind::Intent)? else {
        return Ok(false);
    };
    Ok(intent
        .payload_as::<IntentPayload>()
        .map(|i| i.record_ai_sessions)
        .unwrap_or(false))
}
