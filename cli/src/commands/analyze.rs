//! `madelog analyze` — find patterns in the log and record them as events.
//!
//! Analysis is append-only like everything else: a pattern that was already
//! recorded is not recorded again, so running `madelog analyze` twice leaves the
//! log exactly as it was. Moments are derived on the spot and printed, never
//! stored — the page rebuilds them from the events.

use std::collections::BTreeSet;

use anyhow::{Context, Result};

use crate::event::{Event, EventKind, PatternPayload, Payload};
use crate::moments::{self, Moment};
use crate::patterns;
use crate::Ctx;

pub fn run(ctx: &mut Ctx<'_>, json: bool) -> Result<()> {
    let store = ctx.open_store()?;
    let events = store.all()?;

    let found = patterns::detect(&events);
    let already = recorded(&events);
    let mut added = 0usize;
    for pattern in &found {
        if already.contains(&fingerprint(pattern)) {
            continue;
        }
        store.append_payload(anchor(pattern), &Payload::Pattern(pattern.clone()))?;
        added += 1;
    }

    let moments = moments::pick(moments::candidates(&events, &found));

    if json {
        let body = serde_json::to_string_pretty(&serde_json::json!({
            "patterns": found,
            "moments": moments,
        }))
        .context("rendering the analysis as JSON")?;
        ctx.say(&body)?;
        return Ok(());
    }

    print_patterns(ctx, &found, added)?;
    ctx.blank()?;
    print_moments(ctx, &moments)?;
    Ok(())
}

fn print_patterns(ctx: &mut Ctx<'_>, found: &[PatternPayload], added: usize) -> Result<()> {
    if found.is_empty() {
        ctx.say("no patterns yet — keep committing")?;
        return Ok(());
    }
    ctx.say(&format!("{} in this log:", count(found.len(), "pattern")))?;
    for pattern in found {
        ctx.say(&format!(
            "  {:<13} {:<24} {}",
            patterns::kind_str(pattern.pattern),
            shas(&pattern.commits),
            pattern.detail
        ))?;
    }
    ctx.say(&match added {
        0 => "nothing new to record".to_string(),
        n => format!("recorded {}", count(n, "new pattern event")),
    })?;
    Ok(())
}

fn print_moments(ctx: &mut Ctx<'_>, moments: &[Moment]) -> Result<()> {
    if moments.is_empty() {
        ctx.say("no moments yet — a moment needs a fix after AI, a decision that changed the shape of the project, or a long gap")?;
        return Ok(());
    }
    ctx.say(&format!("{} for the page:", count(moments.len(), "moment")))?;
    for (i, moment) in moments.iter().enumerate() {
        ctx.say(&format!(
            "  {}. {} ({})",
            i + 1,
            moment.title,
            moment.kind.as_str()
        ))?;
        ctx.say(&format!("     {}", moment.body))?;
    }
    Ok(())
}

/// What makes two patterns the same: the rule and the commits it covers.
fn fingerprint(pattern: &PatternPayload) -> String {
    let mut commits: Vec<&str> = pattern.commits.iter().map(String::as_str).collect();
    commits.sort_unstable();
    format!(
        "{} {}",
        patterns::kind_str(pattern.pattern),
        commits.join(" ")
    )
}

fn recorded(events: &[Event]) -> BTreeSet<String> {
    events
        .iter()
        .filter(|e| e.kind == EventKind::Pattern)
        .filter_map(|e| e.payload_as::<PatternPayload>().ok())
        .map(|p| fingerprint(&p))
        .collect()
}

/// A pattern event is anchored to the last commit it is about.
fn anchor(pattern: &PatternPayload) -> Option<&str> {
    pattern
        .commits
        .last()
        .map(String::as_str)
        .filter(|sha| sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()))
}

fn shas(commits: &[String]) -> String {
    let short: Vec<&str> = commits
        .iter()
        .take(3)
        .map(|s| &s[..7.min(s.len())])
        .collect();
    let mut line = short.join(" ");
    if commits.len() > 3 {
        line.push_str(&format!(" +{}", commits.len() - 3));
    }
    line
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}
