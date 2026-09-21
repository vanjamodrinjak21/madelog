//! `madelog summarize` and `madelog summary edit` — weekly summaries (spec §4.3).
//!
//! Summaries are events like everything else: they are appended, never
//! rewritten. Regenerating a week adds a new summary event, editing one adds
//! another, and the page renders the last one per week.

use std::collections::BTreeSet;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::config::Config;
use crate::event::{EventKind, Payload, SummaryPayload};
use crate::summarize::{self, Week};
use crate::Ctx;

/// `madelog summarize [--week N] [--force]`.
pub fn run(ctx: &mut Ctx<'_>, week: Option<u32>, force: bool) -> Result<()> {
    let store = ctx.open_store()?;
    let madelog_dir = ctx.madelog_dir()?;
    let config = Config::load(&madelog_dir.join("config.toml"))?;
    let root = madelog_dir
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| ctx.cwd.clone());
    // The same commits the page counts: the ones git's history still has.
    let (events, _) = crate::page::without_rewritten(store.all()?, &root);

    let weeks = summarize::bucket(&events);
    if weeks.is_empty() {
        ctx.say("nothing to summarise yet — madelog counts weeks from your first commit")?;
        return Ok(());
    }

    if let Some(n) = week {
        if !weeks.iter().any(|w| w.index == n) {
            let known: Vec<String> = weeks.iter().map(|w| w.index.to_string()).collect();
            bail!(
                "this log has no week {n}. Weeks with anything in them: {}",
                known.join(", ")
            );
        }
    }

    let summarised: BTreeSet<String> = summarize::latest_summaries(&events)
        .into_iter()
        .map(|s| s.week_start)
        .collect();

    let todo: Vec<&Week> = weeks
        .iter()
        .filter(|w| week.map_or(true, |n| w.index == n))
        .filter(|w| force || !summarised.contains(&w.week_start()))
        .collect();

    if todo.is_empty() {
        ctx.say(
            "every week already has a summary — run `madelog summarize --force` to write them again",
        )?;
        return Ok(());
    }

    let choice = summarize::choose_provider(&config)?;
    for notice in &choice.notices {
        ctx.say(notice)?;
    }
    if !choice.notices.is_empty() {
        ctx.blank()?;
    }

    for week in todo {
        let written = choice.provider.write(week);
        if let Some(note) = &written.note {
            ctx.say(note)?;
        }
        let event = store.append_payload(
            None,
            &Payload::Summary(SummaryPayload {
                week_start: week.week_start(),
                week_end: week.week_end(),
                text: written.text.clone(),
                generated_by: written.generated_by.clone(),
                edited: false,
            }),
        )?;

        ctx.say(&format!(
            "week {}  {} to {}  (generated)  {}  {}",
            week.index,
            week.week_start(),
            week.week_end(),
            written.generated_by,
            event.id
        ))?;
        ctx.say(&written.text)?;
        ctx.blank()?;
    }

    ctx.say("edit any of them with `madelog summary edit <id>`")?;
    Ok(())
}

/// `madelog summary edit <event-id>` — your words replace the generated ones.
pub fn edit(ctx: &mut Ctx<'_>, event_id: &str) -> Result<()> {
    let store = ctx.open_store()?;
    let Some(target) = store.by_id(event_id)? else {
        bail!("no event with id `{event_id}` in this log");
    };
    if target.kind != EventKind::Summary {
        bail!(
            "`madelog summary edit` takes a summary event; event `{event_id}` is a {}",
            target.kind
        );
    }
    let summary: SummaryPayload = target
        .payload_as()
        .with_context(|| format!("reading summary event `{event_id}`"))?;

    let edited = edit_text(ctx, &summary.text)?;
    let edited = edited.trim();
    if edited.is_empty() {
        bail!("a summary cannot be empty — nothing was changed");
    }
    if edited == summary.text.trim() {
        ctx.say("unchanged — nothing appended")?;
        return Ok(());
    }

    let event = store.append_payload(
        target.commit_sha.as_deref(),
        &Payload::Summary(SummaryPayload {
            week_start: summary.week_start.clone(),
            week_end: summary.week_end.clone(),
            text: edited.to_string(),
            generated_by: "user".to_string(),
            edited: true,
        }),
    )?;

    ctx.say(&format!(
        "week of {} to {}  (edited by you)  {}",
        summary.week_start, summary.week_end, event.id
    ))?;
    ctx.say(edited)?;
    Ok(())
}

/// Open the text in an editor and hand back what came out.
///
/// `MADELOG_EDITOR=stdin` reads the new text from madelog's own input instead, which
/// is how the tests edit a summary without a terminal.
fn edit_text(ctx: &mut Ctx<'_>, original: &str) -> Result<String> {
    let chosen = std::env::var("MADELOG_EDITOR")
        .ok()
        .filter(|v| !v.trim().is_empty());
    if chosen.as_deref() == Some("stdin") {
        let mut text = String::new();
        ctx.input
            .read_to_string(&mut text)
            .context("reading the edited summary")?;
        return Ok(text);
    }

    let path = std::env::temp_dir().join(format!("madelog-summary-{}.md", crate::event::new_id()));
    std::fs::write(&path, original).with_context(|| format!("writing {}", path.display()))?;

    let editor = chosen
        .or_else(|| std::env::var("EDITOR").ok())
        .or_else(|| std::env::var("VISUAL").ok())
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "vi".to_string());
    // `EDITOR="code -w"` is a perfectly normal thing to have.
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let status = Command::new(program)
        .args(parts)
        .arg(&path)
        .status()
        .with_context(|| format!("starting your editor (`{editor}`)"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&path);
        bail!("your editor (`{editor}`) exited without saving; nothing was changed");
    }

    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {} back", path.display()))?;
    let _ = std::fs::remove_file(&path);
    Ok(text)
}
