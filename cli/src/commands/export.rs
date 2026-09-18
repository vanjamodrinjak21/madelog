//! `madelog export` — everything madelog holds about you, as JSON.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::chain;
use crate::config::Config;
use crate::event::{Event, EventKind, IntentPayload};
use crate::Ctx;

#[derive(Debug, Serialize)]
pub struct Export {
    pub version: u32,
    pub exported_at: String,
    pub intent: Option<IntentPayload>,
    pub intent_md: Option<String>,
    pub config: Config,
    pub root: String,
    pub events: Vec<Event>,
}

pub fn build(madelog_dir: &Path) -> Result<Export> {
    let store = crate::store::Store::open(&madelog_dir.join("log.db"))?;
    let events = store.all()?;
    let intent = store
        .first_of_kind(EventKind::Intent)?
        .map(|e| e.payload_as::<IntentPayload>())
        .transpose()
        .context("reading the intent event")?;

    Ok(Export {
        version: 1,
        exported_at: crate::event::now_ts(),
        intent,
        intent_md: std::fs::read_to_string(madelog_dir.join("intent.md")).ok(),
        config: Config::load(&madelog_dir.join("config.toml"))?,
        root: chain::root(&events),
        events,
    })
}

pub fn run(ctx: &mut Ctx<'_>, path: Option<&str>) -> Result<()> {
    let madelog_dir = ctx.madelog_dir()?;
    if !madelog_dir.join("log.db").exists() {
        anyhow::bail!("no madelog log in this repository yet — run `madelog init` first");
    }
    let export = build(&madelog_dir)?;
    let body = serde_json::to_string_pretty(&export).context("rendering the export")?;

    match path {
        Some(path) => {
            let path = ctx.cwd.join(path);
            std::fs::write(&path, format!("{body}\n"))
                .with_context(|| format!("writing {}", path.display()))?;
            ctx.say(&format!(
                "wrote {} events to {}",
                export.events.len(),
                path.display()
            ))?;
        }
        None => ctx.say(&body)?,
    }
    Ok(())
}
