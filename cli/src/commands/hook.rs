//! `madelog hook post-commit`, `post-merge`, `post-rewrite` and `auto` — the
//! only things that run without you asking.
//!
//! None of them may ever fail a git operation. Every error is written to
//! stderr and the command still succeeds.
//!
//! After the commit is in the log, the hook does the rest of the work too:
//! it reads the AI sessions that ran since the last commit, looks for
//! patterns, keeps the weekly summaries current and re-renders the page. That
//! is what makes madelog automatic — nothing is left for you to remember
//! except `madelog note` and `madelog publish`.
//!
//! All of it shares [`Recorder`], which is also what `madelog backfill` uses,
//! so a commit picked up months later carries exactly the same facts as one
//! recorded the second it was made.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};

use crate::capture;
use crate::config::Config;
use crate::event::{CommitPayload, EventKind, IntentPayload, Payload};
use crate::gitrepo::{ExcerptFilter, Repo};
use crate::global::GlobalConfig;
use crate::store::Store;
use crate::Ctx;

/// How far back a hook ever looks for AI sessions.
///
/// A commit made after a week of silence should still pick up the session
/// that produced it; it should not drag a week of unrelated sessions in with
/// it. A day is the line: it covers "I started this yesterday evening" and
/// stops well short of "here is everything since the last release".
pub const MAX_LOOKBACK_HOURS: i64 = 24;

/// How far back a hook always looks, however recently the previous commit was
/// recorded. Two commits a minute apart still come out of one session.
pub const MIN_LOOKBACK_MINUTES: i64 = 5;

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

impl Gap {
    /// Should AI sessions be looked for at all?
    ///
    /// Only for a commit that has just been made. A commit from March did not
    /// come out of a session that ran this afternoon, and today's logs are
    /// the only ones still on the machine.
    fn captures(self) -> bool {
        self == Gap::SinceLastRecord
    }
}

/// Records commits into one repository's log, with the privacy rules and the
/// AI-session opt-in already resolved.
pub struct Recorder<'a> {
    repo: &'a Repo,
    store: Store,
    config: Config,
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
            config,
            filter,
            record_ai_sessions,
        }))
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn config(&self) -> &Config {
        &self.config
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
        // Worked out before the commit goes in, or the "previous recorded
        // commit" would be this one.
        let since = self.window_start(&meta)?;

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

        if self.record_ai_sessions && gap.captures() {
            if let Err(err) = self.record_sessions(sha, since, Utc::now()) {
                eprintln!("madelog: could not read the AI sessions for {sha}: {err:#}");
            }
        }
        Ok(true)
    }

    /// When to start looking for the sessions behind a commit.
    ///
    /// The commit before this one is the natural boundary, so the window
    /// starts when *that* commit was made. Two things bound it:
    ///
    /// - never more than [`MAX_LOOKBACK_HOURS`] ago, so the first commit after
    ///   `madelog backfill` — which recorded months of history a second ago,
    ///   with commit times going back months — does not sweep up everything
    ///   you have ever asked a model;
    /// - never less than [`MIN_LOOKBACK_MINUTES`] before this commit was made,
    ///   so a commit a minute after the last one still sees the session that
    ///   produced it.
    fn window_start(&self, meta: &crate::gitrepo::CommitMeta) -> Result<DateTime<Utc>> {
        let now = Utc::now();
        let floor = now - Duration::hours(MAX_LOOKBACK_HOURS);
        let committed = DateTime::from_timestamp(meta.time, 0).unwrap_or(now);
        let ceiling = committed - Duration::minutes(MIN_LOOKBACK_MINUTES);

        let previous = self
            .store
            .last_of_kind(EventKind::Commit)?
            .and_then(|event| commit_time(&event));
        let start = previous.map_or(floor, |at| at.max(floor));
        Ok(start.min(ceiling))
    }

    /// Read the AI sessions that ran in the window, match what they wrote
    /// against what the commit contains, and append one `ai_session` event
    /// per session.
    ///
    /// Returns how many were appended. Git AI notes, if this machine happens
    /// to have them, are merged in as an extra source; madelog does not need
    /// them and never asks for them.
    pub fn record_sessions(
        &self,
        sha: &str,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Result<usize> {
        let sessions = capture::collect(self.repo, &self.config, since, until);
        let notes = crate::gitai::sessions_for_commit(self.repo.inner(), sha).unwrap_or_default();
        if sessions.is_empty() && notes.is_empty() {
            return Ok(0);
        }
        let diff = self.repo.commit_diff(sha)?;
        let root = self.repo.workdir()?;
        let payloads = capture::attribute(&sessions, &diff, &root, &notes);

        let mut written = 0;
        for payload in payloads {
            self.store
                .append_payload(Some(sha), &Payload::AiSession(payload))?;
            written += 1;
        }
        Ok(written)
    }
}

// ---------------------------------------------------------------------------
// the hooks git calls
// ---------------------------------------------------------------------------

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

/// `madelog hook auto <name>` — what the global hook calls in whatever
/// repository git happens to be in.
///
/// Three outcomes, in order:
///
/// 1. the repository has a `.madelog/`: do exactly what the named hook does;
/// 2. it does not, but you turned on `auto_init` at onboarding: set it up
///    without asking anything, then do the hook;
/// 3. neither: say once, ever, that `madelog init` is a thing, and get out of
///    the way.
pub fn auto(ctx: &mut Ctx<'_>, name: &str) -> Result<()> {
    never_fail("record this commit", auto_inner(ctx, name))
}

fn auto_inner(ctx: &mut Ctx<'_>, name: &str) -> Result<()> {
    // Not a git repository at all: git would not have called us, but a hook
    // is not the place to argue.
    let Ok(repo) = Repo::discover(&ctx.cwd) else {
        return Ok(());
    };
    let Ok(workdir) = repo.workdir() else {
        return Ok(());
    };

    if !workdir.join(".madelog").join("log.db").exists() {
        let global = GlobalConfig::load();
        if !global.auto_init {
            if crate::global::should_hint(&workdir) {
                eprintln!("madelog: run `madelog init` here to record this project");
            }
            return Ok(());
        }
        // Set up quietly: a commit is not the moment for three questions.
        let mut quiet: Vec<u8> = Vec::new();
        let mut silent = Ctx {
            cwd: ctx.cwd.clone(),
            input: &mut std::io::empty(),
            out: &mut quiet,
        };
        crate::commands::init::run(
            &mut silent,
            crate::commands::init::Options {
                record_ai_sessions: Some(global.record_ai_sessions),
                yes: true,
                backfill: false,
                ..Default::default()
            },
        )?;
        eprintln!("madelog: recording {} from now on", workdir.display());
    }

    run_named(ctx, name)
}

fn run_named(ctx: &mut Ctx<'_>, name: &str) -> Result<()> {
    match name {
        "post-rewrite" => {
            let pairs = read_rewrite_pairs(ctx);
            record_rewritten(ctx, &pairs)
        }
        // `post-merge` and anything a future git invents: record HEAD.
        _ => record_head(ctx),
    }
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
    let changed = recorder.record(&sha, Gap::SinceLastRecord)?;
    finish(&repo, &recorder, changed)
}

fn record_rewritten(ctx: &mut Ctx<'_>, new_shas: &[String]) -> Result<()> {
    if new_shas.is_empty() {
        return Ok(());
    }
    let repo = Repo::discover(&ctx.cwd)?;
    let Some(recorder) = Recorder::open(&repo)? else {
        return Ok(());
    };
    let mut changed = false;
    for sha in new_shas {
        changed |= recorder.record(sha, Gap::SinceLastRecord)?;
    }
    finish(&repo, &recorder, changed)
}

/// Analysis, summaries and the page. Never fatal, and never on stdout: a git
/// hook that chatters at every commit is a hook people delete.
fn finish(repo: &Repo, recorder: &Recorder<'_>, changed: bool) -> Result<()> {
    let root = repo.workdir()?;
    if let Err(err) =
        crate::commands::refresh::refresh(&root, recorder.store(), recorder.config(), changed)
    {
        eprintln!("madelog: recorded the commit, but could not refresh the page: {err:#}");
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
    let then = DateTime::parse_from_rfc3339(&previous.ts)
        .with_context(|| format!("event {} has an unreadable timestamp", previous.seq))?;
    let now = DateTime::parse_from_rfc3339(now).context("bad timestamp")?;
    Ok(Some((now - then).num_seconds().max(0)))
}

/// When a recorded commit was made: `committed_at` if it has one, the moment
/// madelog saw it otherwise.
fn commit_time(event: &crate::event::Event) -> Option<DateTime<Utc>> {
    let raw = event
        .payload
        .get("committed_at")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .unwrap_or(&event.ts);
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|at| at.with_timezone(&Utc))
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

/// Why AI sessions are or are not being recorded here, for `status`,
/// `doctor` and `onboard`.
pub fn ai_session_state(repo_root: &Path) -> (bool, String) {
    let db = repo_root.join(".madelog").join("log.db");
    if !db.exists() {
        return (
            false,
            "not recording (no madelog log here — run `madelog init`)".to_string(),
        );
    }
    let opted_in = Store::open(&db)
        .and_then(|store| records_ai_sessions(&store))
        .unwrap_or(false);
    if !opted_in {
        return (
            false,
            "not recording (you said no at `madelog init`; \
             `madelog init --record-ai-sessions` turns it on)"
                .to_string(),
        );
    }
    let detected = capture::report(Some(repo_root));
    if detected.iter().any(|t| t.sessions.unwrap_or(0) > 0) {
        return (true, "recording".to_string());
    }
    if detected.iter().any(|t| t.installed) {
        return (
            true,
            "recording (no sessions have touched this repository yet)".to_string(),
        );
    }
    (
        true,
        "recording, but no AI tool on this machine keeps a log madelog can read".to_string(),
    )
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

    #[test]
    fn only_a_live_commit_goes_looking_for_a_session() {
        assert!(Gap::SinceLastRecord.captures());
        assert!(
            !Gap::Unknown.captures(),
            "history recorded today did not come out of today's sessions"
        );
    }

    #[test]
    fn a_commit_time_is_when_git_made_it_not_when_madelog_saw_it() {
        let event = crate::event::Event {
            seq: 1,
            id: "id".into(),
            ts: "2026-09-18T12:00:00Z".into(),
            kind: EventKind::Commit,
            commit_sha: None,
            payload: serde_json::json!({"committed_at": "2026-03-02T09:00:00Z"}),
            prev_hash: "0".repeat(64),
            hash: "0".repeat(64),
        };
        assert_eq!(
            commit_time(&event).unwrap().to_rfc3339(),
            "2026-03-02T09:00:00+00:00"
        );

        // A commit recorded before madelog carried `committed_at` falls back.
        let older = crate::event::Event {
            payload: serde_json::json!({}),
            ..event
        };
        assert_eq!(
            commit_time(&older).unwrap().to_rfc3339(),
            "2026-09-18T12:00:00+00:00"
        );
    }

    #[test]
    fn a_repository_without_a_log_says_why_it_records_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let (recording, why) = ai_session_state(dir.path());
        assert!(!recording);
        assert!(why.contains("madelog init"), "{why}");
    }
}
