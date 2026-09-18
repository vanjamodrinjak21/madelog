//! `madelog hook post-commit`, `post-merge` and `post-rewrite` — the only things
//! that run without you asking.
//!
//! None of them may ever fail a git operation. Every error is written to
//! stderr and the command still succeeds.
//!
//! All three share [`Recorder`], which is also what `madelog backfill` uses, so
//! a commit picked up months later carries exactly the same facts as one
//! recorded the second it was made.

use anyhow::{Context, Result};

use crate::config::Config;
use crate::event::{CommitPayload, EventKind, IntentPayload, Payload};
use crate::gitrepo::{ExcerptFilter, Repo};
use crate::store::Store;
use crate::Ctx;

/// What `secs_since_prev` should say about a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gap {
    /// Wall-clock seconds since the previously recorded commit event. This is
    /// the live signal: how long the person went between commits.
    SinceLastRecord,
    /// Nothing to say. Backfilled history was recorded all at once, so the
    /// distance between two records is a property of the backfill, not of the
    /// work, and inventing a number would be a lie.
    Unknown,
}

/// Records commits into one repository's log, with the privacy rules and the
/// Git AI opt-in already resolved.
pub struct Recorder<'a> {
    repo: &'a Repo,
    store: Store,
    filter: ExcerptFilter,
    record_ai_sessions: bool,
}

impl<'a> Recorder<'a> {
    /// Open the log of the repository `repo`, or `None` when madelog is not set
    /// up there. A hook may well outlive `.madelog/`, and that is not an error.
    pub fn open(repo: &'a Repo) -> Result<Option<Recorder<'a>>> {
        let madelog_dir = repo.workdir()?.join(".madelog");
        let db = madelog_dir.join("log.db");
        if !db.exists() {
            return Ok(None);
        }
        let store = Store::open(&db)?;
        let config = Config::load(&madelog_dir.join("config.toml"))?;
        let filter = ExcerptFilter::new(&config.privacy.redact)?;
        let record_ai_sessions = records_ai_sessions(&store)?;
        Ok(Some(Recorder {
            repo,
            store,
            filter,
            record_ai_sessions,
        }))
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn records_ai_sessions(&self) -> bool {
        self.record_ai_sessions
    }

    /// Record one commit, unless the log already has it.
    ///
    /// Returns `true` when an event was appended. `ts` is always now: the
    /// chain says when madelog saw the commit, and `committed_at` says when git
    /// made it.
    pub fn record(&self, sha: &str, gap: Gap) -> Result<bool> {
        if self.store.has_commit(sha)? {
            return Ok(false);
        }

        let meta = self.repo.commit_meta(sha)?;
        let files = self.repo.file_changes(sha, &self.filter)?;
        let ts = crate::event::now_ts();
        let secs_since_prev = match gap {
            Gap::SinceLastRecord => seconds_since_previous_commit(&self.store, &ts)?,
            Gap::Unknown => None,
        };

        self.store.append_at(
            ts,
            EventKind::Commit,
            Some(&meta.sha),
            Payload::Commit(CommitPayload {
                sha: meta.sha.clone(),
                message: meta.message.clone(),
                author: meta.author.clone(),
                parents: meta.parents.clone(),
                files,
                secs_since_prev,
                committed_at: crate::event::ts_from_unix(meta.time),
            })
            .to_value(),
        )?;

        if self.record_ai_sessions {
            self.record_ai_sessions_for(sha)?;
        }
        Ok(true)
    }

    /// Git AI's attribution for `sha`, if it wrote any. Never fatal: a commit
    /// that is recorded without its sessions beats a commit that is lost.
    fn record_ai_sessions_for(&self, sha: &str) -> Result<()> {
        match crate::gitai::sessions_for_commit(self.repo.inner(), sha) {
            Ok(sessions) => {
                for session in sessions {
                    self.store
                        .append_payload(Some(sha), &Payload::AiSession(session))?;
                }
            }
            Err(err) => eprintln!("madelog: could not read Git AI notes for {sha}: {err:#}"),
        }
        Ok(())
    }
}

/// `madelog hook post-commit` — the commit that was just made.
pub fn post_commit(ctx: &mut Ctx<'_>) -> Result<()> {
    never_fail("record this commit", record_head(ctx))
}

/// `madelog hook post-merge` — the commit a merge or a pull left at HEAD.
///
/// Commits the merge brought in behind it are not walked: `madelog backfill`
/// exists for history, and a hook that quietly recorded a hundred commits
/// would be a surprise, not a feature.
pub fn post_merge(ctx: &mut Ctx<'_>) -> Result<()> {
    never_fail("record this merge", record_head(ctx))
}

/// `madelog hook post-rewrite` — the commits a rebase or an amend produced.
///
/// git hands us `<old-sha> <new-sha>` pairs on stdin. The new commits are
/// recorded if the log does not have them; the old ones stay in the chain,
/// because the chain records what happened, and they did happen.
pub fn post_rewrite(ctx: &mut Ctx<'_>) -> Result<()> {
    let pairs = read_rewrite_pairs(ctx);
    never_fail(
        "record these rewritten commits",
        record_rewritten(ctx, &pairs),
    )
}

fn never_fail(what: &str, outcome: Result<()>) -> Result<()> {
    if let Err(err) = outcome {
        // A broken hook must not stand between you and your commit.
        eprintln!("madelog: could not {what}: {err:#}");
    }
    Ok(())
}

fn record_head(ctx: &mut Ctx<'_>) -> Result<()> {
    let repo = Repo::discover(&ctx.cwd)?;
    let Some(recorder) = Recorder::open(&repo)? else {
        return Ok(());
    };
    let Some(sha) = repo.head_sha()? else {
        return Ok(());
    };
    recorder.record(&sha, Gap::SinceLastRecord)?;
    Ok(())
}

fn record_rewritten(ctx: &mut Ctx<'_>, new_shas: &[String]) -> Result<()> {
    if new_shas.is_empty() {
        return Ok(());
    }
    let repo = Repo::discover(&ctx.cwd)?;
    let Some(recorder) = Recorder::open(&repo)? else {
        return Ok(());
    };
    for sha in new_shas {
        recorder.record(sha, Gap::SinceLastRecord)?;
    }
    Ok(())
}

/// The new sha of every `<old> <new>` line on stdin, in the order git wrote
/// them. Anything that is not a pair of hex shas is ignored rather than
/// fatal — `post-rewrite` gains fields, and a hook is not the place to be
/// strict about them.
fn read_rewrite_pairs(ctx: &mut Ctx<'_>) -> Vec<String> {
    let mut body = String::new();
    // A hook run by hand has no stdin at all, which is not worth a word.
    let _ = ctx.input.read_to_string(&mut body);
    parse_rewrite_pairs(&body)
}

fn parse_rewrite_pairs(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in body.lines() {
        let mut fields = line.split_whitespace();
        let (Some(_old), Some(new)) = (fields.next(), fields.next()) else {
            continue;
        };
        if is_sha(new) && !out.iter().any(|s| s == new) {
            out.push(new.to_string());
        }
    }
    out
}

fn is_sha(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|c| c.is_ascii_hexdigit())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_pairs_keep_the_new_sha_of_every_line() {
        let old = "a".repeat(40);
        let new = "b".repeat(40);
        let third = "c".repeat(40);
        let body = format!("{old} {new}\n{new} {third}\n");
        assert_eq!(parse_rewrite_pairs(&body), vec![new.clone(), third]);
    }

    #[test]
    fn a_rewritten_sha_is_recorded_once_however_often_git_names_it() {
        let old = "a".repeat(40);
        let new = "b".repeat(40);
        let body = format!("{old} {new}\n{old} {new}\n");
        assert_eq!(parse_rewrite_pairs(&body), vec![new]);
    }

    #[test]
    fn lines_that_are_not_a_pair_of_shas_are_ignored() {
        let good = "b".repeat(40);
        let body = format!(
            "only-one-field\n{} not-a-sha\n{} {good} extra\n",
            "a".repeat(40),
            "a".repeat(40)
        );
        assert_eq!(parse_rewrite_pairs(&body), vec![good]);
        assert!(parse_rewrite_pairs("").is_empty());
    }
}
