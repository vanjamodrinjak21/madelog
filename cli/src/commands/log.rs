//! `madelog log` — what madelog has recorded.

use anyhow::{Context, Result};

use crate::commands::note::short_id;
use crate::Ctx;

pub fn run(ctx: &mut Ctx<'_>, json: bool) -> Result<()> {
    let events = ctx.open_store()?.all()?;

    if json {
        let body = serde_json::to_string_pretty(&events).context("rendering events as JSON")?;
        ctx.say(&body)?;
        return Ok(());
    }

    if events.is_empty() {
        ctx.say("nothing recorded yet")?;
        return Ok(());
    }

    for event in &events {
        let sha = event
            .commit_sha
            .as_deref()
            .map(|s| format!(" {}", &s[..7.min(s.len())]))
            .unwrap_or_default();
        ctx.say(&format!(
            "{:>4}  {}  {}  {:<10}{}  {}",
            event.seq,
            short_id(&event.id),
            event.ts,
            event.kind.as_str(),
            sha,
            event.title()
        ))?;
    }
    Ok(())
}
