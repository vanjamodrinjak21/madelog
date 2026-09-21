//! `madelog export` — everything madelog holds about you, as JSON; or, with
//! `--report`, what the page shows, stamped with the moment it was exported.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::chain;
use crate::config::Config;
use crate::event::{Event, EventKind, IntentPayload};
use crate::page::{self, Published, RatioView, Stats, Verdict};
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
    // The newest intent, which is the one the page and the hook read. A second
    // `madelog init` with new answers supersedes the first.
    let intent = store
        .last_of_kind(EventKind::Intent)?
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

// ---------------------------------------------------------------------------
// the report
// ---------------------------------------------------------------------------

/// The page as data: the same context the template renders, reduced to what
/// a reader or another program needs, and stamped. Nothing in it comes from
/// anywhere but the page context, so the report can never say more than the
/// page does — a private decision or a hidden prompt is absent from both.
#[derive(Debug, Serialize)]
pub struct Report {
    pub version: u32,
    /// `madelog.report`, so a file can be told apart from the raw log export.
    pub kind: &'static str,
    pub exported_at: String,
    pub project: ReportProject,
    pub span: ReportSpan,
    pub stats: Stats,
    /// Absent when no AI session was recorded; the report does not guess.
    pub attribution: Option<ReportAttribution>,
    pub highlights: Vec<String>,
    /// Public decisions, oldest first.
    pub decisions: Vec<ReportDecision>,
    pub moments: Vec<ReportMoment>,
    pub record: ReportRecord,
    pub published: Option<Published>,
}

#[derive(Debug, Serialize)]
pub struct ReportProject {
    pub name: String,
    pub headline: String,
    pub what: String,
    pub for_whom: String,
    pub tools: Vec<String>,
    pub status: String,
    pub status_source: String,
    pub delivered_url: Option<String>,
    pub screenshot: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReportSpan {
    pub first: Option<String>,
    pub last: Option<String>,
    pub days_active: u64,
}

#[derive(Debug, Serialize)]
pub struct ReportAttribution {
    pub ai_pct: u64,
    pub accepted_pct: u64,
    pub reworked_pct: u64,
    pub human_pct: u64,
    pub ai_lines: u64,
    pub accepted_unchanged: u64,
    pub human_reworked: u64,
    pub human_lines: u64,
    pub total_lines: u64,
    /// Commits with a recorded session, which the figures above cover.
    pub covered_commits: u64,
    /// Commits without one, left out of every figure above.
    pub uncovered_commits: u64,
}

#[derive(Debug, Serialize)]
pub struct ReportDecision {
    pub ts: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct ReportMoment {
    pub title: String,
    pub kind: &'static str,
    pub body: String,
    pub generated: bool,
}

#[derive(Debug, Serialize)]
pub struct ReportRecord {
    pub chained_events: u64,
    /// Commits the chain holds that git's history no longer has; not
    /// counted anywhere else in this report.
    pub rewritten_commits: u64,
    pub root: String,
    pub hidden_sessions: u64,
    /// True when a server has confirmed the root; the `published` block then
    /// says which and when.
    pub verified: bool,
}

pub fn build_report(root: &Path) -> Result<Report> {
    let config = Config::load(&root.join(".madelog").join("config.toml"))?;
    let published = crate::commands::refresh::published(&config);
    let ctx = page::build_context(root, published)?;
    let verdict = Verdict::of(&ctx);
    let ratio = ctx.ratio.map(|r| {
        let view = RatioView::of(r);
        ReportAttribution {
            ai_pct: view.ai_pct,
            accepted_pct: view.accepted_pct,
            reworked_pct: view.reworked_pct,
            human_pct: view.human_pct,
            ai_lines: r.ai_lines,
            accepted_unchanged: r.accepted_unchanged,
            human_reworked: r.human_reworked,
            human_lines: r.human_lines,
            total_lines: view.total,
            covered_commits: r.covered_commits,
            uncovered_commits: r.uncovered_commits,
        }
    });

    Ok(Report {
        version: 1,
        kind: "madelog.report",
        exported_at: crate::event::now_ts(),
        project: ReportProject {
            name: ctx.project.name.clone(),
            headline: verdict.headline.clone(),
            what: ctx.project.what.clone(),
            for_whom: ctx.project.for_whom.clone(),
            tools: ctx.project.tools.clone(),
            status: ctx.project.status.clone(),
            status_source: ctx.project.status_source.clone(),
            delivered_url: ctx.project.delivered_url.clone(),
            screenshot: ctx.project.screenshot.clone(),
        },
        span: ReportSpan {
            first: ctx.project.first_ts.clone(),
            last: ctx.project.last_ts.clone(),
            days_active: ctx.stats.days_active,
        },
        stats: ctx.stats,
        attribution: ratio,
        highlights: verdict.highlights,
        decisions: ctx
            .timeline
            .iter()
            .filter(|e| e.kind == "decision")
            .map(|e| ReportDecision {
                ts: e.ts.clone(),
                text: e.title.clone(),
            })
            .collect(),
        moments: ctx
            .moments
            .iter()
            .map(|m| ReportMoment {
                title: m.title.clone(),
                kind: m.kind.as_str(),
                body: m.body.clone(),
                generated: m.generated,
            })
            .collect(),
        record: ReportRecord {
            chained_events: ctx.chained_events,
            rewritten_commits: ctx.rewritten_commits,
            root: ctx.badge.root.clone(),
            hidden_sessions: ctx.hidden_sessions,
            verified: ctx.badge.published.is_some(),
        },
        published: ctx.badge.published,
    })
}

pub fn run(ctx: &mut Ctx<'_>, path: Option<&str>, report: bool) -> Result<()> {
    let madelog_dir = ctx.madelog_dir()?;
    if !madelog_dir.join("log.db").exists() {
        anyhow::bail!("no madelog log in this repository yet — run `madelog init` first");
    }

    let (body, wrote) = if report {
        let root = madelog_dir
            .parent()
            .context("the .madelog directory has a parent")?
            .to_path_buf();
        let report = build_report(&root)?;
        let body = serde_json::to_string_pretty(&report).context("rendering the report")?;
        (
            body,
            format!(
                "wrote the report ({} commits, status {}, exported {})",
                report.stats.commits, report.project.status, report.exported_at
            ),
        )
    } else {
        let export = build(&madelog_dir)?;
        let body = serde_json::to_string_pretty(&export).context("rendering the export")?;
        let n = export.events.len();
        (body, format!("wrote {n} events"))
    };

    match path {
        Some(path) => {
            let path = ctx.cwd.join(path);
            std::fs::write(&path, format!("{body}\n"))
                .with_context(|| format!("writing {}", path.display()))?;
            ctx.say(&format!("{wrote} to {}", path.display()))?;
        }
        None => ctx.say(&body)?,
    }
    Ok(())
}
