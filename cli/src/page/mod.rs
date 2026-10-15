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

pub use tmpl::{RatioView, Verdict};

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
    /// `active | paused | delivered | archived` when the author set one,
    /// `delivered | active | dormant` when derived from the log. Empty only
    /// for a context that never went through [`build_context`].
    pub status: String,
    /// `set` or `derived`.
    pub status_source: String,
}

/// The number of days without a commit after which a project that nobody
/// marked otherwise reads as dormant.
pub const DORMANT_AFTER_DAYS: i64 = 14;

/// The project's status and where it came from.
///
/// The author's word wins. Failing that, a delivered link means delivered, a
/// commit inside the last [`DORMANT_AFTER_DAYS`] days means active, and
/// anything older is dormant. A log with no commits at all is active: the
/// project has just started.
pub fn status(config: &Config, events: &[Event]) -> (String, String) {
    status_at(config, events, Utc::now())
}

fn status_at(config: &Config, events: &[Event], now: DateTime<Utc>) -> (String, String) {
    if let Some(set) = config
        .project
        .status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return (set.to_ascii_lowercase(), "set".into());
    }
    if non_empty(config.project.delivered_url.as_deref()).is_some() {
        return ("delivered".into(), "derived".into());
    }
    let last_commit = events
        .iter()
        .filter(|e| e.kind == EventKind::Commit)
        .filter_map(|e| parse_ts(&effective_ts(e)))
        .max();
    let derived = match last_commit {
        Some(at) if (now - at).num_days() > DORMANT_AFTER_DAYS => "dormant",
        _ => "active",
    };
    (derived.into(), "derived".into())
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

/// How many lines the AI proposed and how many of them survived, counted
/// only over the commits that carry a recorded AI session. A commit with no
/// session is not attributed either way: nothing recorded says whether a
/// model was involved, so the page does not guess.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Ratio {
    pub ai_lines: u64,
    pub accepted_unchanged: u64,
    pub human_reworked: u64,
    /// Lines added by the covered commits that no session proposed.
    pub human_lines: u64,
    /// Commits with at least one recorded session.
    pub covered_commits: u64,
    /// Commits with none, whose lines are left out of every figure above.
    pub uncovered_commits: u64,
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
    /// Every event in the chain, which is what `madelog verify` counts. The
    /// timeline shows fewer: intents, patterns, summaries, hides and private
    /// decisions are chained but not drawn.
    pub chained_events: u64,
    /// Commits the chain recorded that git's history no longer contains,
    /// because a rebase, an amend or a purge rewrote them away. They stay in
    /// the chain and are counted in `chained_events`; nothing else on the
    /// page counts them.
    pub rewritten_commits: u64,
    /// What the repository's manifests say it is built with (`stack::detect`).
    pub stack: Vec<String>,
    pub badge: Badge,
}

/// Build the page context from the repo's `.madelog` log.
pub fn build_context(root: &Path, published: Option<Published>) -> Result<PageContext> {
    let madelog_dir = root.join(".madelog");
    let store = Store::open(&madelog_dir.join("log.db"))?;
    let chain_root = store.root()?;
    let all_events = store.all()?;
    let config = Config::load(&madelog_dir.join("config.toml"))?;

    // The badge and the event count are about the chain, so they see every
    // event. Everything else is about the work, so commits that git no
    // longer has are set aside first.
    let first_ts = all_events.first().map(|e| e.ts.clone());
    let last_ts = all_events.last().map(|e| e.ts.clone());
    let chained_events = all_events.len() as u64;
    let (events, rewritten_commits) = without_rewritten(all_events, root);

    // The badge is about the chain, so it keeps `ts`: it certifies when
    // madelog recorded things. Everything the reader is told about the work
    // itself — the header dates, the timeline, the day count — uses
    // `committed_at` where a commit has one, so a repository that madelog
    // joined late still shows the months it really took.
    let (first_at, last_at) = span_of_work(&events);

    let project = project(root, &config, &events, &first_at, &last_at);
    let hidden = hidden_session_ids(&events);
    let timeline = timeline(&events, &hidden, &first_at, &last_at);
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
        chained_events,
        rewritten_commits,
        stack: crate::stack::detect(root),
        badge: Badge {
            root: chain_root,
            first_ts,
            last_ts,
            published,
        },
    })
}

/// The events minus the commits that git's history no longer contains, and
/// the sessions anchored to them; plus how many commits that was.
///
/// Only a commit git still holds but no longer reaches from HEAD counts as
/// rewritten: that is the one case git can vouch for. A sha git does not
/// have at all is kept, because "pruned by `git gc`" and "recorded on another
/// machine" look the same from here, and setting work aside on a guess is
/// the one thing this page must never do. Outside a git repository nothing
/// is set aside either.
pub fn without_rewritten(events: Vec<Event>, root: &Path) -> (Vec<Event>, u64) {
    let Ok(repo) = crate::gitrepo::Repo::discover(root) else {
        return (events, 0);
    };
    let mut gone: BTreeSet<String> = BTreeSet::new();
    for event in &events {
        if event.kind != EventKind::Commit {
            continue;
        }
        let Ok(payload) = event.payload_as::<CommitPayload>() else {
            continue;
        };
        if !repo.has_commit(&payload.sha) {
            continue;
        }
        if let Ok(false) = repo.reachable_from_head(&payload.sha) {
            gone.insert(payload.sha);
        }
    }
    if gone.is_empty() {
        return (events, 0);
    }
    let kept = events
        .into_iter()
        .filter(|e| match e.kind {
            EventKind::Commit => e
                .payload
                .get("sha")
                .and_then(|v| v.as_str())
                .map(|sha| !gone.contains(sha))
                .unwrap_or(true),
            EventKind::AiSession => e
                .commit_sha
                .as_deref()
                .map(|sha| !gone.contains(sha))
                .unwrap_or(true),
            _ => true,
        })
        .collect();
    (kept, gone.len() as u64)
}

/// When a commit event says the work happened: `committed_at` if it has one,
/// the moment madelog recorded it otherwise.
///
/// Events that are not commits have nothing but their own timestamp, and that
/// is the right answer for them: a decision was written when it was written.
fn effective_ts(event: &Event) -> String {
    if event.kind == EventKind::Commit {
        if let Some(at) = event
            .payload
            .get("committed_at")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
        {
            return at.to_string();
        }
    }
    event.ts.clone()
}

/// The earliest and latest moment the record covers, by [`effective_ts`].
///
/// Not `events.first()` and `events.last()`: backfilled commits are recorded
/// newest-to-oldest in wall-clock terms only by accident, and a note written
/// today about a commit from March must not stretch the axis backwards.
fn span_of_work(events: &[Event]) -> (Option<String>, Option<String>) {
    let mut stamps: Vec<(DateTime<Utc>, String)> = events
        .iter()
        .filter_map(|event| {
            let ts = effective_ts(event);
            parse_ts(&ts).map(|at| (at, ts))
        })
        .collect();
    stamps.sort_by(|a, b| a.0.cmp(&b.0));
    (
        stamps.first().map(|(_, ts)| ts.clone()),
        stamps.last().map(|(_, ts)| ts.clone()),
    )
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

    let (status, status_source) = status(config, events);

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
        status,
        status_source,
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
                // The day the commit was made, not the day madelog heard
                // about it: a backfilled month of work is a month of days.
                if let Some(ts) = parse_ts(&effective_ts(event)) {
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
                    ts: effective_ts(event),
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
    // Chain order is the order madelog *recorded* things, and `madelog
    // backfill` records a year of commits after this morning's. The page is
    // about the work, so it reads in the order the work happened; a stable
    // sort keeps chain order for anything that shares a timestamp.
    out.sort_by(|a, b| match (parse_ts(&a.ts), parse_ts(&b.ts)) {
        (Some(x), Some(y)) => x.cmp(&y),
        _ => std::cmp::Ordering::Equal,
    });
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
    let mut covered: BTreeSet<String> = BTreeSet::new();

    for event in events {
        if event.kind != EventKind::AiSession {
            continue;
        }
        let Ok(payload) = event.payload_as::<AiSessionPayload>() else {
            continue;
        };
        sessions += 1;
        if let Some(sha) = event.commit_sha.as_deref() {
            covered.insert(sha.to_string());
        }
        for file in &payload.files {
            ai_lines += file.ai_lines;
            human_reworked += file.ai_lines_modified_by_human;
        }
    }
    if sessions == 0 {
        return None;
    }

    // Only the commits a session was recorded for say anything about who
    // wrote their lines. The rest are counted, not attributed.
    let mut added = 0u64;
    let mut covered_commits = 0u64;
    let mut uncovered_commits = 0u64;
    for event in events {
        if event.kind != EventKind::Commit {
            continue;
        }
        let Ok(payload) = event.payload_as::<CommitPayload>() else {
            continue;
        };
        if covered.contains(&payload.sha) {
            covered_commits += 1;
            added += payload.files.iter().map(|f| f.added).sum::<u64>();
        } else {
            uncovered_commits += 1;
        }
    }

    let human_reworked = human_reworked.min(ai_lines);
    Some(Ratio {
        ai_lines,
        accepted_unchanged: ai_lines - human_reworked,
        human_reworked,
        human_lines: added.saturating_sub(ai_lines),
        covered_commits,
        uncovered_commits,
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

    fn commit_at(committed_at: &str) -> Event {
        Event {
            seq: 1,
            id: "e1".into(),
            ts: "2026-01-01T00:00:00Z".into(),
            kind: EventKind::Commit,
            commit_sha: Some("abc".into()),
            payload: serde_json::json!({
                "sha": "abc", "message": "m", "author": "a", "parents": [],
                "files": [], "committed_at": committed_at
            }),
            prev_hash: String::new(),
            hash: String::new(),
        }
    }

    #[test]
    fn status_is_the_authors_word_or_derived_from_the_log() {
        let now = parse_ts("2026-09-21T10:00:00Z").unwrap();
        let mut config = Config::default();

        // Nothing at all: a project that has just started is active.
        assert_eq!(
            status_at(&config, &[], now),
            ("active".to_string(), "derived".to_string())
        );

        // A commit inside the window keeps it active; older is dormant. The
        // commit's own date counts, not the day madelog recorded it.
        let recent = commit_at("2026-09-10T09:00:00Z");
        let old = commit_at("2026-08-01T09:00:00Z");
        assert_eq!(status_at(&config, &[old.clone(), recent], now).0, "active");
        assert_eq!(status_at(&config, &[old], now).0, "dormant");

        // A delivered link means delivered, however old the last commit.
        config.project.delivered_url = Some("https://a.test".into());
        assert_eq!(
            status_at(&config, &[commit_at("2025-01-01T00:00:00Z")], now),
            ("delivered".to_string(), "derived".to_string())
        );

        // The author's word wins over everything.
        config.project.status = Some("Paused".into());
        assert_eq!(
            status_at(&config, &[], now),
            ("paused".to_string(), "set".to_string())
        );
        config.project.status = Some("  ".into());
        assert_eq!(status_at(&config, &[], now).1, "derived");
    }
}
