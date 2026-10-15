//! `madelog pin` — put one public decision first on the printed sheet and in
//! the headline. `--remove` takes it back off. The log only grows: pinning
//! and unpinning are both events.

use anyhow::{bail, Result};

use crate::commands::note::find_decision;
use crate::event::{DecisionPayload, EventKind, Payload, PinPayload};
use crate::page::{pinned_order, resolve_amendments};
use crate::Ctx;

pub fn run(ctx: &mut Ctx<'_>, decision_id: &str, remove: bool) -> Result<()> {
    let store = ctx.open_store()?;
    let target = find_decision(&store, decision_id)?;

    let resolved = resolve_amendments(store.all()?);
    let root = resolved
        .roots
        .get(&target.id)
        .cloned()
        .unwrap_or_else(|| target.id.clone());
    let current = resolved
        .events
        .iter()
        .find(|e| e.kind == EventKind::Decision && e.id == root)
        .and_then(|e| e.payload_as::<DecisionPayload>().ok());
    if !remove && current.map(|d| d.private).unwrap_or(true) {
        bail!(
            "decision `{}` is private; a private decision is never on the page, so it cannot be pinned",
            target.id
        );
    }

    let is_pinned = pinned_order(&resolved.events, &resolved.roots).contains(&root);
    if is_pinned != remove {
        ctx.say(if remove {
            "not pinned. Nothing was recorded."
        } else {
            "already pinned. Nothing was recorded."
        })?;
        return Ok(());
    }

    store.append_payload(
        target.commit_sha.as_deref(),
        &Payload::Pin(PinPayload {
            target_id: target.id.clone(),
            pinned: !remove,
        }),
    )?;
    ctx.say(if remove {
        "unpinned. The decision goes back to its place in time order."
    } else {
        "pinned. It now leads the printed sheet and the page headline."
    })?;
    Ok(())
}
