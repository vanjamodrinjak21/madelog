//! `madelog note` — one sentence, anchored to the commit you are on, or to the
//! one you name with `--at`.

use anyhow::{bail, Context, Result};

use crate::event::{DecisionPayload, Payload};
use crate::gitrepo::Repo;
use crate::Ctx;

pub fn run(ctx: &mut Ctx<'_>, text: &str, private: bool, at: Option<&str>) -> Result<()> {
    let text = text.trim();
    if text.is_empty() {
        bail!("a note needs a sentence: madelog note \"why you did it that way\"");
    }
    let store = ctx.open_store()?;
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
    ctx.say(&format!("noted{anchor}{visibility}"))?;
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
