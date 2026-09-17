//! `pop note` — one sentence, anchored to the commit you are on.

use anyhow::{bail, Result};

use crate::event::{DecisionPayload, Payload};
use crate::gitrepo::Repo;
use crate::Ctx;

pub fn run(ctx: &mut Ctx<'_>, text: &str, private: bool) -> Result<()> {
    let text = text.trim();
    if text.is_empty() {
        bail!("a note needs a sentence: pop note \"why you did it that way\"");
    }
    let store = ctx.open_store()?;
    let head = Repo::discover(&ctx.cwd)?.head_sha()?;
    let event = store.append_payload(
        head.as_deref(),
        &Payload::Decision(DecisionPayload {
            text: text.to_string(),
            private,
        }),
    )?;

    let anchor = match &event.commit_sha {
        Some(sha) => format!(" at {}", &sha[..7.min(sha.len())]),
        None => String::new(),
    };
    let visibility = if private { " (private, never published)" } else { "" };
    ctx.say(&format!("noted{anchor}{visibility}"))?;
    Ok(())
}
