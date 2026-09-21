//! The project page: the design mock's context renders, and a real repo's log
//! renders the same template without leaking anything it must not show.

mod common;

use std::path::{Path, PathBuf};

use madelog::event::{AiSessionFile, AiSessionPayload, Payload};
use madelog::page::{self, PageContext, Published};
use madelog::store::Store;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli/ has a parent")
        .to_path_buf()
}

fn mock_context() -> PageContext {
    let path = repo_root().join("design/mock/context.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&raw).expect("design/mock/context.json matches PageContext")
}

/// Every `<details>` opened is closed, and there is exactly one document.
fn assert_html_basics(html: &str) {
    assert_eq!(html.matches("<html").count(), 1, "one <html>");
    assert_eq!(html.matches("</html>").count(), 1, "one </html>");
    assert_eq!(html.matches("<body").count(), 1, "one <body>");
    assert_eq!(
        html.matches("<details").count(),
        html.matches("</details>").count(),
        "balanced <details>"
    );
    assert_eq!(
        html.matches("<svg").count(),
        html.matches("</svg>").count(),
        "balanced <svg>"
    );
    assert!(html.starts_with("<!doctype html>"), "starts with a doctype");
    assert!(html.trim_end().ends_with("</html>"), "ends with </html>");
}

/// The page must never fetch anything but the Google Fonts stylesheet.
fn assert_self_contained(html: &str) {
    for marker in [
        "<script src=",
        "<link rel=\"stylesheet\" href=\"/",
        "<iframe",
    ] {
        assert!(!html.contains(marker), "page pulls in {marker}");
    }
    let externals = html.matches("https://fonts.").count();
    assert!(externals >= 2, "the Google Fonts link stays");
}

// ---------------------------------------------------------------------------
// the design mock
// ---------------------------------------------------------------------------

#[test]
fn design_mock_context_deserialises_and_renders() {
    let ctx = mock_context();
    assert_eq!(ctx.project.name, "madelog");
    assert_eq!(ctx.stats.commits, 45);
    assert_eq!(ctx.timeline.len(), 71);
    assert_eq!(ctx.moments.len(), 4);
    assert_eq!(ctx.hidden_sessions, 1);
    assert_eq!(ctx.chained_events, 84);

    let html = page::render(&ctx).expect("mock renders");
    assert_html_basics(&html);
    assert_self_contained(&html);

    // 1. intent and what was delivered
    assert!(html.contains("<title>madelog · how it came together</title>"));
    assert!(html.contains(
        "A Rust CLI that records how a project came together and turns it into one page"
    ));
    assert!(html.contains("Students and juniors applying for a first job"));
    assert!(html.contains("github.com/vanjamodrinjak21/madelog"));
    assert!(html.contains("<span>claude-code</span>"));

    // 1b. the verdict: what a reader gets in six seconds. The AI share leads,
    // the span sits under the day count, and the three lines are facts the
    // log supports.
    assert!(html.contains("<div class=\"lead\"><b>39%</b><span>AI-written lines</span></div>"));
    assert!(html.contains("<b>45</b><span>commits</span>"));
    assert!(html.contains("days active · 3 Sep – 17 Sep</span>"));
    assert!(html.contains("<ul class=\"highlights\">"));
    assert!(html.contains("<li>Reworked 830 of 2,140 AI-written lines by hand</li>"));
    assert!(html.contains("<li>Decided: "));
    assert!(html.contains(
        "<li>45 commits recorded as they happened, 84 events hash-chained, root confirmed by the server</li>"
    ));
    // The header carries the root next to the verified mark.
    assert!(html.contains("Verified record <code>8a1f5c0e</code>"));
    // The prose that used to fill the first screen is gone.
    assert!(!html.contains("nothing was written after the fact"));

    // 2. timeline: rendered server-side, no JS needed to read it
    assert!(html.contains("<svg class=\"tl\""));
    assert!(html.contains("class=\"tl-pt tl-commit\""));
    assert!(html.contains("class=\"tl-pt tl-decision\""));
    assert!(html.contains("class=\"tl-pt tl-ai_session\""));
    assert!(html.contains("class=\"tl-pt tl-hidden\""));
    assert!(html.contains("Every record <span class=\"count\">(71)</span>"));
    // The record line counts the chain, not the timeline: 84 events, of which
    // 71 are drawn.
    assert!(html.contains("Hash-chained log of 84 events"), "{html}");
    // x = 0.1676 sits at cx 229.2 on the 78..980 axis
    assert!(html.contains("cx=\"229.2\""), "x maps onto the axis");
    assert!(html.contains("<text x=\"117.2\" y=\"134\" class=\"tl-day\">4 Sep</text>"));
    assert!(html.contains("class=\"tl-day tl-day-major\">7 Sep</text>"));

    // 3. moments
    assert!(html.contains("<h2 id=\"mo-h\">Four moments</h2>"));
    assert!(html.contains("A HashMap from Claude broke the hash chain"));
    assert!(
        html.contains(">generated</span>"),
        "generated text is marked"
    );
    assert!(html.contains("<a href=\"#ev-01994c10-7a2e-7016-8b1c-9e000001eef0\">"));

    // 4. ratio
    assert!(html.contains("5,560 lines"));
    assert!(html.contains("2,140"));
    assert!(html.contains("width:24%"));
    assert!(html.contains("width:15%"));
    assert!(html.contains("width:61%"));

    // 5. badge
    assert!(html.contains("Verified record"));
    assert!(html.contains("8a1f5c0e2b7d4e6f9a3c1b8d7e5f2a4c6b9d0e1f3a5c7b9d2e4f6a8c0b1d3e5c3e7"));
    assert!(html.contains("https://madelog.app/v1/verify/quiet-heron-k7m2"));
    assert!(html.contains("1 session hidden"));
    assert!(!html.contains("Not yet published"));

    // summaries
    assert!(html.contains("1 Sep – 7 Sep"));
    assert!(html.contains("generated locally (qwen3:4b)"));
    assert!(html.contains("generated locally (qwen3:4b), edited"));
    assert!(html.contains("summary generated by remote model (claude-sonnet-4-5)"));
}

#[test]
fn empty_states_render() {
    let mut ctx = mock_context();
    ctx.ratio = None;
    ctx.summaries.clear();
    ctx.moments.truncate(2);
    ctx.hidden_sessions = 0;
    ctx.badge.published = None;
    ctx.project.delivered_url = None;

    let html = page::render(&ctx).expect("empty states render");
    assert_html_basics(&html);
    assert!(html.contains("No AI attribution was recorded for this project."));
    assert!(!html.contains("width:24%"));
    assert!(html.contains("No weekly summaries"));
    assert!(html.contains("<h2 id=\"mo-h\">Two moments</h2>"));
    assert!(!html.contains("session hidden by the author before publishing"));
    assert!(html.contains("Not yet published"));
    assert!(!html.contains(">Verified record<"));
    // No delivered link means no Delivered row: the first screen never
    // opens with a "not yet".
    assert!(!html.contains("not yet published"));
    assert!(!html.contains("<dt>Delivered</dt>"));
    // No attribution means no lead figure, and the highlights start with the
    // decision instead.
    assert!(!html.contains("class=\"lead\""));
    assert!(html.contains("<ul class=\"highlights\">\n      <li>Decided: "));
    assert!(
        html.contains("84 events hash-chained</li>"),
        "unpublished: no server clause"
    );
}

#[test]
fn no_moments_at_all_still_renders() {
    let mut ctx = mock_context();
    ctx.moments.clear();
    let html = page::render(&ctx).expect("renders without moments");
    assert_html_basics(&html);
    assert!(html.contains("No moments"));
}

// ---------------------------------------------------------------------------
// a real repository
// ---------------------------------------------------------------------------

/// Build a small but complete log: commits, a public note, a private note, an
/// AI session that gets hidden, analysis and summaries.
fn seeded_repo() -> common::TestRepo {
    let repo = common::init_repo();
    common::init_madelog(&repo);
    madelog::run_capture(repo.root(), &["config", "set", "llm.provider", "none"]).unwrap();

    let mut shas = Vec::new();
    for (n, message) in [
        "feat(store): one events table, no ORM",
        "feat(chain): sha256 over canonical json",
        "fix(chain): HashMap made canonical json unstable; BTreeMap instead",
        "feat(page): tera template skeleton",
    ]
    .iter()
    .enumerate()
    {
        repo.write(
            &format!("src/file{n}.rs"),
            &format!("// {message}\nfn f{n}() {{ println!(\"{n}\"); }}\n"),
        );
        let sha = repo.commit_all(message);
        madelog::run_capture(repo.root(), &["hook", "post-commit"]).unwrap();
        shas.push(sha);
    }

    madelog::run_capture(
        repo.root(),
        &[
            "note",
            "Every payload struct uses BTreeMap so the hash is stable.",
        ],
    )
    .unwrap();
    madelog::run_capture(
        repo.root(),
        &[
            "note",
            "--private",
            "SUPERSECRETNOTE: the client pays late and I am looking elsewhere.",
        ],
    )
    .unwrap();

    // An AI session anchored to the second commit, which we then hide.
    let store = Store::open(&repo.path(".madelog/log.db")).unwrap();
    let session = store
        .append_payload(
            Some(&shas[1]),
            &Payload::AiSession(AiSessionPayload {
                tool: "claude-code".into(),
                model: Some("claude-sonnet-4-5".into()),
                session_id: "sess-1".into(),
                prompt: Some("HIDDENPROMPTTEXT: implement a hash chain over events".into()),
                response_excerpt: None,
                files: vec![AiSessionFile {
                    path: "src/file1.rs".into(),
                    ai_lines: 40,
                    ai_lines_modified_by_human: 12,
                }],
                accepted: true,
            }),
        )
        .unwrap();
    drop(store);

    madelog::run_capture(repo.root(), &["analyze"]).unwrap();
    madelog::run_capture(repo.root(), &["summarize"]).unwrap();
    madelog::run_capture(repo.root(), &["hide", &session.id]).unwrap();
    repo
}

#[test]
fn render_local_writes_a_page_that_keeps_its_secrets() {
    let repo = seeded_repo();
    let path = page::render_local(repo.root(), None).expect("render_local");
    assert_eq!(path, repo.path(".madelog/site/index.html"));
    assert!(path.exists(), "the file is on disk");

    let html = std::fs::read_to_string(&path).unwrap();
    assert_html_basics(&html);
    assert_self_contained(&html);

    // privacy
    assert!(
        !html.contains("SUPERSECRETNOTE"),
        "a private decision reached the page"
    );
    assert!(
        !html.contains("HIDDENPROMPTTEXT"),
        "a hidden session's prompt reached the page"
    );

    // the public note did
    assert!(html.contains("Every payload struct uses BTreeMap so the hash is stable."));

    // the first screen: a capitalised one-line headline, a one-day span, and
    // three facts the log supports; counts read as English
    assert!(html.contains("<h1 id=\"intent-h\">A small cli that records process</h1>"));
    assert!(html.contains("<b>1</b><span>day active · "));
    assert!(html.contains("<li>Reworked 12 of 40 AI-written lines by hand</li>"));
    assert!(html
        .contains("<li>Decided: Every payload struct uses BTreeMap so the hash is stable.</li>"));
    assert!(html.contains("<li>4 commits recorded as they happened, "));
    assert!(html.contains("4 commits, 1 decision, 1 AI session on one axis."));

    // status: derived, and a commit just now means active; the paper stamp
    // carries it too
    assert!(html.contains(
        "<span class=\"status status-active\" title=\"Status derived from the log\">active</span>"
    ));
    assert!(html.contains("<p class=\"stamp\" aria-hidden=\"true\">"));
    assert!(html.contains(" · active · record to "));
    assert!(html.contains(" · unpublished local render · root "));

    // hidden session
    assert!(html.contains("tl-pt tl-hidden"));
    assert!(html.contains("hidden session"));
    assert!(html.contains("1 session hidden"));

    // not published yet
    assert!(html.contains("Not yet published"));
    assert!(html.contains("madelog publish --local"));

    // the chain root
    let root = Store::open(&repo.path(".madelog/log.db"))
        .unwrap()
        .root()
        .unwrap();
    assert_eq!(root.len(), 64);
    assert!(html.contains(&root), "the chain root is on the page");

    // summaries were generated and are marked as such
    assert!(html.contains("class=\"week\""), "a week summary rendered");
    assert!(html.contains("class=\"gen\""), "generated text is marked");

    // commits and their diff excerpts
    assert!(html.contains("feat(store): one events table, no ORM"));
    assert!(html.contains("class=\"diff\""));
}

#[test]
fn build_context_matches_the_log() {
    let repo = seeded_repo();
    let ctx = page::build_context(repo.root(), None).expect("build_context");

    assert_eq!(
        ctx.project.name,
        repo.root().file_name().unwrap().to_string_lossy()
    );
    assert_eq!(ctx.project.what, "a small cli that records process");
    assert_eq!(ctx.project.for_whom, "a studio hiring juniors");
    assert_eq!(ctx.project.tools, vec!["claude-code", "cursor"]);

    assert_eq!(ctx.stats.commits, 4);
    assert_eq!(ctx.stats.decisions, 1, "private decisions are not counted");
    assert_eq!(ctx.stats.ai_sessions, 1);
    assert_eq!(ctx.stats.days_active, 1);
    assert_eq!(ctx.project.status, "active", "a commit today is active");
    assert_eq!(ctx.project.status_source, "derived");

    // one hidden session, no private decision, no intent/summary/pattern rows
    assert_eq!(ctx.hidden_sessions, 1);
    let kinds: Vec<&str> = ctx.timeline.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds.iter().filter(|k| **k == "commit").count(), 4);
    assert_eq!(kinds.iter().filter(|k| **k == "decision").count(), 1);
    assert_eq!(kinds.iter().filter(|k| **k == "hidden").count(), 1);
    assert_eq!(kinds.iter().filter(|k| **k == "ai_session").count(), 0);
    assert!(ctx.timeline.iter().all(|e| (0.0..=1.0).contains(&e.x)
        && !e.detail.as_deref().unwrap_or("").is_empty()
        || e.detail.is_none()));

    let ratio = ctx.ratio.expect("one session means a ratio");
    assert_eq!(ratio.ai_lines, 40);
    assert_eq!(ratio.human_reworked, 12);
    assert_eq!(ratio.accepted_unchanged, 28);

    assert_eq!(ctx.badge.root.len(), 64);
    assert!(ctx.badge.published.is_none());
    assert!(ctx.project.first_ts.is_some());
    assert!(ctx.project.last_ts.is_some());
    assert!(!ctx.summaries.is_empty());
}

/// The number on the page and the number `madelog verify` prints are the same
/// number: every chained event, not just the ones the timeline draws.
#[test]
fn the_record_line_counts_every_chained_event() {
    let repo = seeded_repo();
    let ctx = page::build_context(repo.root(), None).expect("build_context");
    let all = Store::open(&repo.path(".madelog/log.db"))
        .unwrap()
        .all()
        .unwrap();

    assert_eq!(ctx.chained_events, all.len() as u64);
    assert!(
        ctx.chained_events > ctx.timeline.len() as u64,
        "intents, patterns, summaries and hides are chained but not drawn: \
         {} chained, {} on the timeline",
        ctx.chained_events,
        ctx.timeline.len()
    );

    let verify = madelog::run_capture(repo.root(), &["verify"]).unwrap();
    assert!(
        verify.contains(&format!("{} events", ctx.chained_events)),
        "verify says {verify:?}, the page says {}",
        ctx.chained_events
    );

    let html = page::render(&ctx).expect("renders");
    assert!(
        html.contains(&format!(
            "Hash-chained log of {} events",
            ctx.chained_events
        )),
        "{html}"
    );

    // The badge still spans every event, so the dates and the count agree.
    assert_eq!(ctx.badge.first_ts.as_deref(), Some(all[0].ts.as_str()));
    assert_eq!(
        ctx.badge.last_ts.as_deref(),
        Some(all[all.len() - 1].ts.as_str())
    );
}

#[test]
fn a_published_page_shows_the_badge() {
    let repo = seeded_repo();
    let published = Published {
        slug: "quiet-heron-k7m2".into(),
        url: "https://madelog.app/p/quiet-heron-k7m2".into(),
        received_at: "2026-09-17T14:02:41Z".into(),
        verify_url: "https://madelog.app/v1/verify/quiet-heron-k7m2".into(),
    };
    let path = page::render_local(repo.root(), Some(published)).expect("render_local");
    let html = std::fs::read_to_string(path).unwrap();
    assert!(html.contains("Verified record"));
    assert!(html.contains("https://madelog.app/v1/verify/quiet-heron-k7m2"));
    assert!(html.contains("madelog.app/p/quiet-heron-k7m2"));
    assert!(!html.contains("Not yet published"));
}

#[test]
fn an_empty_log_still_renders() {
    let repo = common::init_repo();
    common::init_madelog(&repo);
    let path = page::render_local(repo.root(), None).expect("render_local on a bare log");
    let html = std::fs::read_to_string(path).unwrap();
    assert_html_basics(&html);
    assert!(html.contains("Not yet published"));
    assert!(html.contains("No AI attribution was recorded for this project."));
}
