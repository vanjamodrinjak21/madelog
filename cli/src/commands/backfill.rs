//! `madelog backfill` — the history this repository had before madelog arrived.
//!
//! Installing madelog halfway through a project used to mean a page that said
//! "1 day active", because the log started the day the hook did. Backfill
//! walks first parents from HEAD and records every commit the log is missing,
//! oldest first, with the same stats and excerpts the hook would have written.
//!
//! What it does *not* do is pretend. The `ts` of a backfilled event is now,
//! because now is when madelog saw the commit; `committed_at` carries the real
//! commit time, and `secs_since_prev` is left empty, because the distance
//! between two records made in the same second says nothing about the work.

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use git2::Oid;

use crate::commands::hook::{Gap, Recorder};
use crate::gitrepo::Repo;
use crate::Ctx;

/// At most this many commits in one run. A repository with more history than
/// this wants `--since`, not a twenty-minute wait and a 40 MB page.
pub const MAX_COMMITS: usize = 2000;

#[derive(Debug, Default)]
pub struct Options {
    /// A sha or a date; the oldest commit to record, inclusive.
    pub since: Option<String>,
    pub yes: bool,
}

/// Where the walk should stop.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Since {
    /// Everything reachable along first parents.
    Beginning,
    /// This commit and everything after it.
    Commit(String),
    /// Everything committed on or after this local date.
    Date(NaiveDate),
}

pub fn run(ctx: &mut Ctx<'_>, opts: Options) -> Result<()> {
    let repo = Repo::discover(&ctx.cwd)?;
    let workdir = repo.workdir()?;
    if !workdir.join(".madelog").join("log.db").exists() {
        bail!(
            "there is no madelog log in {} — run `madelog init` first. `madelog backfill` records \
             history into an existing log; it does not start one.",
            workdir.display()
        );
    }

    let since = parse_since(&repo, opts.since.as_deref())?;
    let Some(head) = repo.head_sha()? else {
        ctx.say("no commits in this repository yet, so there is no history to record")?;
        return Ok(());
    };

    let Some(recorder) = Recorder::open(&repo)? else {
        bail!("the log in {} could not be opened", workdir.display());
    };

    let walked = first_parents(&repo, &head, &since)?;
    let missing: Vec<String> = walked
        .iter()
        .filter(|sha| !recorder.store().has_commit(sha).unwrap_or(false))
        .cloned()
        .collect();

    if missing.is_empty() {
        ctx.say(&format!(
            "nothing to record: all {} commit{} on this branch are already in the log",
            walked.len(),
            if walked.len() == 1 { "" } else { "s" }
        ))?;
        return Ok(());
    }

    let capped = missing.len() > MAX_COMMITS;
    let chosen: Vec<String> = missing
        .iter()
        .rev()
        .take(MAX_COMMITS)
        .rev()
        .cloned()
        .collect();

    ctx.say(&format!(
        "{} commit{} in this repository {} not recorded yet.",
        missing.len(),
        if missing.len() == 1 { "" } else { "s" },
        if missing.len() == 1 { "is" } else { "are" }
    ))?;
    if capped {
        ctx.say(&format!(
            "Recording the newest {MAX_COMMITS}. Run `madelog backfill --since <date|sha>` for the rest."
        ))?;
    }

    if !opts.yes && !ctx.ask_yes_no("Record them now?", true)? {
        ctx.say("Nothing changed.")?;
        return Ok(());
    }

    // Oldest first, so the chain reads in the order the work happened.
    let mut recorded = 0usize;
    let mut failed = 0usize;
    for sha in &chosen {
        match recorder.record(sha, Gap::Unknown) {
            Ok(true) => recorded += 1,
            Ok(false) => {}
            Err(err) => {
                failed += 1;
                eprintln!("madelog: could not record {}: {err:#}", short(sha));
            }
        }
    }

    ctx.blank()?;
    ctx.say(&format!(
        "recorded {recorded} commit{}, oldest first",
        if recorded == 1 { "" } else { "s" }
    ))?;
    if let (Some(first), Some(last)) = (chosen.first(), chosen.last()) {
        ctx.say(&format!("  from {}  to {}", short(first), short(last)))?;
    }
    if failed > 0 {
        ctx.say(&format!(
            "  {failed} could not be read and were left out; the details are on stderr"
        ))?;
    }
    if recorder.records_ai_sessions() {
        ctx.say("  Git AI notes were read for every one of them")?;
    }
    ctx.blank()?;
    ctx.say(
        "note: backfilled commits carry no secs_since_prev signal. They were all recorded just \
         now, so the gaps between records say nothing about the gaps between commits; the page \
         uses committed_at for the timeline instead.",
    )?;
    Ok(())
}

/// Every commit on the first-parent line from HEAD that the log does not
/// have, oldest first. What `madelog status` counts and `madelog backfill`
/// records.
pub fn unrecorded(repo: &Repo, store: &crate::store::Store) -> Result<Vec<String>> {
    let Some(head) = repo.head_sha()? else {
        return Ok(Vec::new());
    };
    Ok(first_parents(repo, &head, &Since::Beginning)?
        .into_iter()
        .filter(|sha| !store.has_commit(sha).unwrap_or(false))
        .collect())
}

/// First-parent shas from HEAD back to `since`, oldest first.
fn first_parents(repo: &Repo, head: &str, since: &Since) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut current = Some(Oid::from_str(head).context("HEAD is not a commit id")?);

    while let Some(oid) = current {
        let commit = repo
            .inner()
            .find_commit(oid)
            .with_context(|| format!("no commit {oid}"))?;
        let sha = commit.id().to_string();

        match since {
            Since::Commit(stop) if &sha == stop => {
                out.push(sha);
                break;
            }
            Since::Date(from) => {
                let date = crate::event::ts_from_unix(commit.time().seconds())
                    .and_then(|ts| local_date(&ts));
                // An unreadable commit time is not a reason to stop walking,
                // but it is a reason not to claim the commit is recent.
                if date.is_some_and(|d| d < *from) {
                    break;
                }
                out.push(sha);
            }
            _ => out.push(sha),
        }

        current = commit.parent_ids().next();
        // A walk that is already impossibly long stops before it eats the
        // machine; the cap above decides what is kept.
        if out.len() > MAX_COMMITS * 4 {
            break;
        }
    }

    out.reverse();
    Ok(out)
}

/// `--since` is a sha if git knows one, and a date otherwise.
fn parse_since(repo: &Repo, raw: Option<&str>) -> Result<Since> {
    let Some(raw) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(Since::Beginning);
    };
    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Ok(Since::Date(date));
    }
    match repo.inner().revparse_single(raw) {
        Ok(object) => Ok(Since::Commit(
            object
                .peel_to_commit()
                .with_context(|| format!("`{raw}` is not a commit"))?
                .id()
                .to_string(),
        )),
        Err(_) => bail!(
            "`--since {raw}` is neither a commit this repository has nor a date like 2026-01-31"
        ),
    }
}

fn local_date(ts: &str) -> Option<NaiveDate> {
    use chrono::{DateTime, Local};
    Some(
        DateTime::parse_from_rfc3339(ts)
            .ok()?
            .with_timezone(&Local)
            .date_naive(),
    )
}

fn short(sha: &str) -> String {
    sha[..7.min(sha.len())].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_date_is_a_date_and_nothing_else_is() {
        let dir = tempfile::TempDir::new().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let repo = Repo::discover(dir.path()).unwrap();

        assert_eq!(parse_since(&repo, None).unwrap(), Since::Beginning);
        assert_eq!(parse_since(&repo, Some("  ")).unwrap(), Since::Beginning);
        assert_eq!(
            parse_since(&repo, Some("2026-01-31")).unwrap(),
            Since::Date(NaiveDate::from_ymd_opt(2026, 1, 31).unwrap())
        );
        let err = parse_since(&repo, Some("deadbeef")).unwrap_err();
        assert!(format!("{err:#}").contains("2026-01-31"), "{err:#}");
    }
}
