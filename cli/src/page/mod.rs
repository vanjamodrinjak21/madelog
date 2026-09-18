//! Project page generator (task E fills this in; task G consumes it).
//!
//! Contract shared by both tasks — keep these signatures stable:
//! - [`PageContext`] mirrors the "Shared page context" JSON in the plan.
//! - [`build_context`] derives it from the event log and config.
//! - [`render`] renders the Tera template to a single self-contained HTML string.
//! - [`render_local`] writes `.madelog/site/index.html` and returns its path.
//!
//! Everything the page can show has to be in [`PageContext`] first. The
//! template has no access to the log, so a private decision or a redacted
//! prompt cannot reach the page by accident: it is dropped here, once.

mod tmpl;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::event::{
    AiSessionPayload, CommitPayload, DecisionPayload, Event, EventKind, HidePayload, IntentPayload,
    SummaryPayload,
};
use crate::moments::Moment;
use crate::store::Store;

/// Server-side publication facts shown in the badge; `None` for local renders.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Published {
    pub slug: String,
    pub url: String,
    pub received_at: String,
    pub verify_url: String,
}

/// What the project set out to be, and what came of it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Project {
    pub name: String,
    /// The `what` of the intent event.
    pub what: String,
    pub for_whom: String,
    pub tools: Vec<String>,
    pub delivered_url: Option<String>,
    pub screenshot: Option<String>,
    /// The timestamp of the first and last event; `None` for an empty log.
    pub first_ts: Option<String>,
    pub last_ts: Option<String>,
}

/// The size of the record, in the four numbers the page prints.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Stats {
    pub commits: u64,
    /// Private decisions are not counted; they are not on the page either.
    pub decisions: u64,
    pub ai_sessions: u64,
    /// Distinct local dates that carry at least one commit.
    pub days_active: u64,
}

/// One point on the timeline and its record below it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TimelineEntry {
    pub id: String,
    pub ts: String,
    /// `commit | decision | ai_session | hidden | pattern`
    pub kind: String,
    pub title: String,
    /// Diff excerpt for a commit, prompt for a session, nothing otherwise.
    pub detail: Option<String>,
    pub sha: Option<String>,
    /// Position on the axis, 0 at the first event and 1 at the last.
    pub x: f64,
}

/// How many lines the AI proposed and how many of them survived.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Ratio {
    pub ai_lines: u64,
    pub accepted_unchanged: u64,
    pub human_reworked: u64,
    pub human_lines: u64,
}

/// The verification badge at the foot of the page.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Badge {
    /// The chain root: 64 hex characters.
    pub root: String,
    pub first_ts: Option<String>,
    pub last_ts: Option<String>,
    pub published: Option<Published>,
}

/// Everything the template needs, and nothing it must not see.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PageContext {
    pub project: Project,
    pub stats: Stats,
    pub timeline: Vec<TimelineEntry>,
    pub moments: Vec<Moment>,
    pub ratio: Option<Ratio>,
    pub summaries: Vec<SummaryPayload>,
    pub hidden_sessions: u64,
    pub badge: Badge,
}

/// Build the page context from the repo's `.madelog` log.
pub fn build_context(root: &Path, published: Option<Published>) -> Result<PageContext> {
    let madelog_dir = root.join(".madelog");
    let store = Store::open(&madelog_dir.join("log.db"))?;
    let chain_root = store.root()?;
    let events = store.all()?;
    let config = Config::load(&madelog_dir.join("config.toml"))?;

    let first_ts = events.first().map(|e| e.ts.clone());
    let last_ts = events.last().map(|e| e.ts.clone());

    let project = project(root, &config, &events, &first_ts, &last_ts);
    let hidden = hidden_session_ids(&events);
    let timeline = timeline(&events, &hidden, &first_ts, &last_ts);
    let hidden_sessions = timeline.iter().filter(|e| e.kind == "hidden").count() as u64;

    let patterns = crate::patterns::detect(&events);
    let moments = crate::moments::pick(crate::moments::candidates(&events, &patterns));

    Ok(PageContext {
        project,
        stats: stats(&events),
        timeline,
        moments,
        ratio: ratio(&events),
        summaries: crate::summarize::latest_summaries(&events),
        hidden_sessions,
        badge: Badge {
            root: chain_root,
            first_ts,
            last_ts,
            published,
        },
    })
}

/// Render the page to HTML.
pub fn render(ctx: &PageContext) -> Result<String> {
    tmpl::render(ctx)
}

/// Render and write `.madelog/site/index.html`, returning its path.
pub fn render_local(root: &Path, published: Option<Published>) -> Result<PathBuf> {
    let ctx = build_context(root, published)?;
    let html = render(&ctx)?;
    let dir = root.join(".madelog").join("site");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("index.html");
    std::fs::write(&path, html).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// the parts
// ---------------------------------------------------------------------------

fn project(
    root: &Path,
    config: &Config,
    events: &[Event],
    first_ts: &Option<String>,
    last_ts: &Option<String>,
) -> Project {
    let intent = events
        .iter()
        .rev()
        .find(|e| e.kind == EventKind::Intent)
        .and_then(|e| e.payload_as::<IntentPayload>().ok());

    let name = if config.project.name.trim().is_empty() {
        root.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".to_string())
    } else {
        config.project.name.trim().to_string()
    };

    Project {
        name,
        what: intent.as_ref().map(|i| i.what.clone()).unwrap_or_default(),
        for_whom: intent
            .as_ref()
            .map(|i| i.for_whom.clone())
            .unwrap_or_default(),
        tools: intent.map(|i| i.tools).unwrap_or_default(),
        delivered_url: non_empty(config.project.delivered_url.as_deref()),
        screenshot: non_empty(config.project.screenshot.as_deref()),
        first_ts: first_ts.clone(),
        last_ts: last_ts.clone(),
    }
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// The ids of the AI sessions the author hid before publishing.
fn hidden_session_ids(events: &[Event]) -> BTreeSet<String> {
    events
        .iter()
        .filter(|e| e.kind == EventKind::Hide)
        .filter_map(|e| e.payload_as::<HidePayload>().ok())
        .map(|p| p.target_id)
        .collect()
}

fn stats(events: &[Event]) -> Stats {
    let mut days: BTreeSet<String> = BTreeSet::new();
    let mut stats = Stats::default();
    for event in events {
        match event.kind {
            EventKind::Commit => {
                stats.commits += 1;
                if let Some(ts) = parse_ts(&event.ts) {
                    days.insert(ts.with_timezone(&Local).format("%Y-%m-%d").to_string());
                }
            }
            EventKind::Decision => {
                let private = event
                    .payload_as::<DecisionPayload>()
                    .map(|p| p.private)
                    .unwrap_or(true);
                if !private {
                    stats.decisions += 1;
                }
            }
            EventKind::AiSession => stats.ai_sessions += 1,
            _ => {}
        }
    }
    stats.days_active = days.len() as u64;
    stats
}

fn timeline(
    events: &[Event],
    hidden: &BTreeSet<String>,
    first_ts: &Option<String>,
    last_ts: &Option<String>,
) -> Vec<TimelineEntry> {
    let span = match (
        first_ts.as_deref().and_then(parse_ts),
        last_ts.as_deref().and_then(parse_ts),
    ) {
        (Some(a), Some(b)) => Some((a, (b - a).num_milliseconds())),
        _ => None,
    };

    let mut out = Vec::new();
    for event in events {
        let entry = match event.kind {
            EventKind::Commit => {
                let Ok(payload) = event.payload_as::<CommitPayload>() else {
                    continue;
                };
                Some(TimelineEntry {
                    id: event.id.clone(),
                    ts: event.ts.clone(),
                    kind: "commit".into(),
                    title: first_line(&payload.message),
                    detail: diff_excerpt(&payload),
                    sha: Some(short_sha(&payload.sha)),
                    x: 0.0,
                })
            }
            EventKind::Decision => {
                let Ok(payload) = event.payload_as::<DecisionPayload>() else {
                    continue;
                };
                if payload.private {
                    continue;
                }
                Some(TimelineEntry {
                    id: event.id.clone(),
                    ts: event.ts.clone(),
                    kind: "decision".into(),
                    title: payload.text.trim().to_string(),
                    detail: None,
                    sha: None,
                    x: 0.0,
                })
            }
            EventKind::AiSession => {
                let Ok(payload) = event.payload_as::<AiSessionPayload>() else {
                    continue;
                };
                if hidden.contains(&event.id) {
                    Some(TimelineEntry {
                        id: event.id.clone(),
                        ts: event.ts.clone(),
                        kind: "hidden".into(),
                        title: "AI session hidden by the author".into(),
                        detail: None,
                        sha: None,
                        x: 0.0,
                    })
                } else {
                    Some(TimelineEntry {
                        id: event.id.clone(),
                        ts: event.ts.clone(),
                        kind: "ai_session".into(),
                        title: format!("{} session", payload.tool),
                        detail: payload
                            .prompt
                            .map(|p| p.trim().to_string())
                            .filter(|p| !p.is_empty()),
                        sha: event.commit_sha.as_deref().map(short_sha),
                        x: 0.0,
                    })
                }
            }
            _ => None,
        };
        if let Some(mut entry) = entry {
            entry.x = position(&entry.ts, span);
            out.push(entry);
        }
    }
    out
}

/// Where a timestamp sits on a 0..1 axis running from the first to the last event.
fn position(ts: &str, span: Option<(DateTime<Utc>, i64)>) -> f64 {
    let Some((start, millis)) = span else {
        return 0.0;
    };
    if millis <= 0 {
        return 0.0;
    }
    let Some(at) = parse_ts(ts) else {
        return 0.0;
    };
    let x = (at - start).num_milliseconds() as f64 / millis as f64;
    (x.clamp(0.0, 1.0) * 10_000.0).round() / 10_000.0
}

/// The diff excerpt of a commit, one block per file that has one.
fn diff_excerpt(payload: &CommitPayload) -> Option<String> {
    let mut blocks: Vec<String> = Vec::new();
    for file in &payload.files {
        if let Some(excerpt) = file.diff_excerpt.as_deref() {
            let excerpt = excerpt.trim_end_matches('\n');
            if excerpt.is_empty() {
                continue;
            }
            blocks.push(format!("@@ {}\n{}", file.path, excerpt));
        }
    }
    if blocks.is_empty() {
        None
    } else {
        Some(blocks.join("\n"))
    }
}

fn ratio(events: &[Event]) -> Option<Ratio> {
    let mut sessions = 0u64;
    let mut ai_lines = 0u64;
    let mut human_reworked = 0u64;
    let mut added = 0u64;

    for event in events {
        match event.kind {
            EventKind::AiSession => {
                let Ok(payload) = event.payload_as::<AiSessionPayload>() else {
                    continue;
                };
                sessions += 1;
                for file in &payload.files {
                    ai_lines += file.ai_lines;
                    human_reworked += file.ai_lines_modified_by_human;
                }
            }
            EventKind::Commit => {
                if let Ok(payload) = event.payload_as::<CommitPayload>() {
                    added += payload.files.iter().map(|f| f.added).sum::<u64>();
                }
            }
            _ => {}
        }
    }

    if sessions == 0 {
        return None;
    }
    let human_reworked = human_reworked.min(ai_lines);
    Some(Ratio {
        ai_lines,
        accepted_unchanged: ai_lines - human_reworked,
        human_reworked,
        human_lines: added.saturating_sub(ai_lines),
    })
}

/// A label for every event a moment refers to, so the template can name its
/// links without searching the timeline.
fn ref_labels(ctx: &PageContext) -> BTreeMap<String, String> {
    let by_id: BTreeMap<&str, &TimelineEntry> =
        ctx.timeline.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut out = BTreeMap::new();
    for moment in &ctx.moments {
        for id in &moment.refs {
            let label = match by_id.get(id.as_str()) {
                Some(entry) => format!(
                    "{} {}",
                    tmpl::day_month(&entry.ts),
                    tmpl::kind_label(&entry.kind)
                ),
                None => "record".to_string(),
            };
            out.insert(id.clone(), label);
        }
    }
    out
}

fn parse_ts(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

fn short_sha(sha: &str) -> String {
    sha[..7.min(sha.len())].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_compiles() {
        tmpl::tera().expect("cli/templates/project.html.tera compiles");
    }

    #[test]
    fn an_empty_context_renders() {
        let html = render(&PageContext::default()).expect("renders");
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("Not yet published"));
    }

    #[test]
    fn x_runs_from_zero_to_one() {
        let start = parse_ts("2026-09-03T00:00:00Z").unwrap();
        let span = Some((start, 86_400_000));
        assert_eq!(position("2026-09-03T00:00:00Z", span), 0.0);
        assert_eq!(position("2026-09-03T12:00:00Z", span), 0.5);
        assert_eq!(position("2026-09-04T00:00:00Z", span), 1.0);
        assert_eq!(position("2026-09-05T00:00:00Z", span), 1.0);
        assert_eq!(position("2026-09-03T00:00:00Z", None), 0.0);
    }

    #[test]
    fn a_ratio_needs_a_session() {
        assert!(ratio(&[]).is_none());
    }
}
