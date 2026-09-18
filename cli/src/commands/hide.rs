//! `madelog hide` — keep one AI session off the page without removing it from the
//! chain. The page says "1 session hidden".

use anyhow::{bail, Result};

use crate::event::{EventKind, HidePayload, Payload};
use crate::Ctx;

pub fn run(ctx: &mut Ctx<'_>, event_id: &str) -> Result<()> {
    let store = ctx.open_store()?;
    let Some(target) = store.by_id(event_id)? else {
        bail!("no event with id `{event_id}` in this log");
    };
    if target.kind != EventKind::AiSession {
        bail!(
            "only ai_session events can be hidden; event `{event_id}` is a {}",
            target.kind
        );
    }

    // Hiding is idempotent: the page already says the session is hidden, and a
    // second `hide` event would only lengthen the chain for nothing.
    if already_hidden(&store, &target.id)? {
        ctx.say("already hidden. Nothing was recorded; the page already leaves this session out.")?;
        return Ok(());
    }

    store.append_payload(
        target.commit_sha.as_deref(),
        &Payload::Hide(HidePayload {
            target_kind: EventKind::AiSession.as_str().to_string(),
            target_id: target.id.clone(),
        }),
    )?;
    ctx.say("hidden. The session stays in the chain; the page will say one session is hidden.")?;
    Ok(())
}

/// Is there already a `hide` event pointing at this session?
fn already_hidden(store: &crate::store::Store, target_id: &str) -> Result<bool> {
    Ok(store
        .all()?
        .iter()
        .filter(|e| e.kind == EventKind::Hide)
        .filter_map(|e| e.payload_as::<HidePayload>().ok())
        .any(|p| p.target_id == target_id))
}
