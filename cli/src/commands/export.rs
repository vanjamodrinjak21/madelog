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
    Ok(build_report_with_line(root)?.0)
}

/// The report and the page's one-sentence account of how AI was used.
fn build_report_with_line(root: &Path) -> Result<(Report, String)> {
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

    let ai_line = verdict.ai_line.clone();
    let report = Report {
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
    };
    Ok((report, ai_line))
}

/// `2026-09-17T10:00:00Z` as `17 Sep 2026`; the raw text when it does not parse.
fn day(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|d| d.format("%-d %b %Y").to_string())
        .unwrap_or_else(|_| ts.to_string())
}

/// Decisions listed before the rest are summed up.
const MD_DECISIONS: usize = 6;

/// The report as Markdown to paste into an application. Only what the report
/// holds; a section with nothing to say is left out.
pub fn report_markdown(r: &Report, ai_line: &str) -> String {
    let mut out = format!("# {}\n\n", r.project.name);
    if !r.project.headline.trim().is_empty() {
        out.push_str(&format!("{}\n\n", r.project.headline.trim()));
    }
    let mut meta = vec![r.project.status.clone()];
    match (&r.span.first, &r.span.last) {
        (Some(a), Some(b)) => meta.push(format!("{} – {}", day(a), day(b))),
        (Some(a), None) | (None, Some(a)) => meta.push(day(a)),
        (None, None) => {}
    }
    meta.retain(|m| !m.trim().is_empty());
    if !meta.is_empty() {
        out.push_str(&format!("{}\n\n", meta.join(" · ")));
    }

    out.push_str(&format!("- {} commits\n", r.stats.commits));
    out.push_str(&format!("- {} days active\n", r.span.days_active));
    out.push_str(&format!("- {} decisions\n", r.stats.decisions));
    match &r.attribution {
        Some(a) if a.uncovered_commits > 0 => out.push_str(&format!(
            "- {}% of lines written by AI (over {} of {} commits with a recorded AI session)\n",
            a.ai_pct,
            a.covered_commits,
            a.covered_commits + a.uncovered_commits
        )),
        Some(a) => out.push_str(&format!(
            "- {}% of lines written by AI ({} AI sessions)\n",
            a.ai_pct, r.stats.ai_sessions
        )),
        None => {}
    }

    if !r.decisions.is_empty() {
        out.push_str("\n## What I decided\n\n");
        for d in r.decisions.iter().take(MD_DECISIONS) {
            out.push_str(&format!("- {}\n", d.text.trim()));
        }
        if r.decisions.len() > MD_DECISIONS {
            out.push_str(&format!(
                "- + {} more in the record\n",
                r.decisions.len() - MD_DECISIONS
            ));
        }
    }

    if !ai_line.trim().is_empty() {
        out.push_str(&format!("\n## How AI was used\n\n{}\n", ai_line.trim()));
    }

    out.push_str("\n## Record\n\n");
    out.push_str(&format!(
        "- {} chained events, root `{}`\n",
        r.record.chained_events,
        r.record.root.chars().take(12).collect::<String>()
    ));
    match &r.published {
        Some(p) => out.push_str(&format!("- [{}]({})\n", p.url, p.url)),
        None => out.push_str("- Not yet verified by a server\n"),
    }

    out.push_str(&format!("\n_Exported {} by madelog._\n", r.exported_at));
    out
}

pub fn run(ctx: &mut Ctx<'_>, path: Option<&str>, report: bool, md: bool) -> Result<()> {
    let madelog_dir = ctx.madelog_dir()?;
    if !madelog_dir.join("log.db").exists() {
        anyhow::bail!("no madelog log in this repository yet — run `madelog init` first");
    }

    let (body, wrote) = if report {
        let root = madelog_dir
            .parent()
            .context("the .madelog directory has a parent")?
            .to_path_buf();
        let (report, ai_line) = build_report_with_line(&root)?;
        if md {
            (
                report_markdown(&report, &ai_line),
                "wrote the report as Markdown".to_string(),
            )
        } else {
            let body = serde_json::to_string_pretty(&report).context("rendering the report")?;
            (
                body,
                format!(
                    "wrote the report ({} commits, status {}, exported {})",
                    report.stats.commits, report.project.status, report.exported_at
                ),
            )
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn report(decisions: usize, published: Option<Published>, uncovered: u64) -> Report {
        Report {
            version: 1,
            kind: "madelog.report",
            exported_at: "2026-10-15T09:00:00Z".into(),
            project: ReportProject {
                name: "pop".into(),
                headline: "A small cli".into(),
                what: "a small cli".into(),
                for_whom: "studios".into(),
                tools: vec![],
                status: "active".into(),
                status_source: "derived".into(),
                delivered_url: None,
                screenshot: None,
            },
            span: ReportSpan {
                first: Some("2026-09-17T10:00:00Z".into()),
                last: Some("2026-10-03T18:30:00Z".into()),
                days_active: 9,
            },
            stats: Stats {
                commits: 42,
                decisions: decisions as u64,
                ai_sessions: 5,
                days_active: 9,
            },
            attribution: Some(ReportAttribution {
                ai_pct: 61,
                accepted_pct: 40,
                reworked_pct: 21,
                human_pct: 39,
                ai_lines: 600,
                accepted_unchanged: 400,
                human_reworked: 200,
                human_lines: 390,
                total_lines: 990,
                covered_commits: 30,
                uncovered_commits: uncovered,
            }),
            highlights: vec![],
            decisions: (1..=decisions)
                .map(|i| ReportDecision {
                    ts: "2026-09-18T10:00:00Z".into(),
                    text: format!("Decision {i}"),
                })
                .collect(),
            moments: vec![],
            record: ReportRecord {
                chained_events: 120,
                rewritten_commits: 0,
                root: "abcdef0123456789abcdef".into(),
                hidden_sessions: 0,
                verified: published.is_some(),
            },
            published,
        }
    }

    #[test]
    fn markdown_has_only_what_the_report_holds() {
        let md = report_markdown(&report(8, None, 12), "AI wrote 61% of the lines.");
        let expected = "# pop

A small cli

active · 17 Sep 2026 – 3 Oct 2026

- 42 commits
- 9 days active
- 8 decisions
- 61% of lines written by AI (over 30 of 42 commits with a recorded AI session)

## What I decided

- Decision 1
- Decision 2
- Decision 3
- Decision 4
- Decision 5
- Decision 6
- + 2 more in the record

## How AI was used

AI wrote 61% of the lines.

## Record

- 120 chained events, root `abcdef012345`
- Not yet verified by a server

_Exported 2026-10-15T09:00:00Z by madelog._
";
        assert_eq!(md, expected);
    }

    #[test]
    fn markdown_links_the_published_page_and_omits_empty_sections() {
        let p = Published {
            slug: "pop".into(),
            url: "https://example.test/p/pop".into(),
            received_at: "2026-10-15T09:00:00Z".into(),
            verify_url: "https://example.test/v/pop".into(),
        };
        let md = report_markdown(&report(0, Some(p), 0), "");
        assert!(!md.contains("## What I decided"));
        assert!(!md.contains("## How AI was used"));
        assert!(md.contains("- 61% of lines written by AI (5 AI sessions)"));
        assert!(md.contains("- [https://example.test/p/pop](https://example.test/p/pop)"));
    }
}
