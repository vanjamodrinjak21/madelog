//! `madelog note` — one sentence, anchored to the commit you are on, or to the
//! one you name with `--at`.

use anyhow::{bail, Context, Result};

use crate::event::{DecisionPayload, Event, EventKind, Payload};
use crate::gitrepo::Repo;
use crate::store::Store;
use crate::Ctx;

pub fn run(
    ctx: &mut Ctx<'_>,
    text: &str,
    private: bool,
    at: Option<&str>,
    amends: Option<&str>,
) -> Result<()> {
    let text = text.trim();
    if text.is_empty() {
        bail!("a note needs a sentence: madelog note \"why you did it that way\"");
    }
    let store = ctx.open_store()?;
    let amends = match amends {
        Some(id) => Some(find_decision(&store, id)?.id),
        None => None,
    };
    let repo = Repo::discover(&ctx.cwd)?;
    let anchor_sha = match at {
        Some(rev) => Some(resolve(&repo, rev)?),
        None => repo.head_sha()?,
    };
    let event = store.append_payload(
        anchor_sha.as_deref(),
        &Payload::Decision(DecisionPayload {
            text: text.to_string(),
            private,
            amends: amends.clone(),
        }),
    )?;

    let anchor = match &event.commit_sha {
        Some(sha) => format!(" at {}", &sha[..7.min(sha.len())]),
        None => String::new(),
    };
    let visibility = if private {
        " (private, never published)"
    } else {
        ""
    };
    let verb = if amends.is_some() { "amended" } else { "noted" };
    ctx.say(&format!("{verb}{anchor}{visibility}"))?;
    Ok(())
}

/// The full sha of whatever `rev` names — a short sha, a full one, a tag, a
/// branch. A note anchored to a commit that does not exist would be a note
/// nothing can ever find, so an unknown revision is an error, not a HEAD note.
fn resolve(repo: &Repo, rev: &str) -> Result<String> {
    let object = repo
        .inner()
        .revparse_single(rev)
        .with_context(|| format!("no commit `{rev}` in this repository"))?;
    let commit = object
        .peel_to_commit()
        .with_context(|| format!("`{rev}` is not a commit"))?;
    Ok(commit.id().to_string())
}

/// The decision event `id` names: its full id, or a prefix of exactly one
/// decision's id. Anything else is an error, so a correction can never land
/// on the wrong sentence.
pub(crate) fn find_decision(store: &Store, id: &str) -> Result<Event> {
    let id = id.trim();
    if id.is_empty() {
        bail!("name a decision by its id; `madelog log` lists them");
    }
    if let Some(event) = store.by_id(id)? {
        if event.kind != EventKind::Decision {
            bail!("event `{id}` is a {}, not a decision", event.kind);
        }
        return Ok(event);
    }
    let mut matches: Vec<Event> = store
        .all()?
        .into_iter()
        .filter(|e| e.kind == EventKind::Decision && e.id.starts_with(id))
        .collect();
    match matches.len() {
        0 => bail!("no decision with id `{id}` in this log"),
        1 => Ok(matches.remove(0)),
        n => bail!("`{id}` matches {n} decisions; give more of the id"),
    }
}
