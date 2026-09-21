//! The Tera side of the page: one template, compiled once, plus the handful
//! of filters it needs.
//!
//! `tera` is pulled in without its default features, so there is no `date`
//! filter and no `now()`. Every derived value is therefore either a filter
//! registered here or a key added to the context in [`render`]; the template
//! itself does no arithmetic it cannot be trusted with.

use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Datelike, NaiveDate, Utc, Weekday};
use serde::Serialize;
use tera::{Context, Tera, Value};

use super::PageContext;

const TEMPLATE: &str = include_str!("../../templates/project.html.tera");
const NAME: &str = "project.html.tera";

/// The left and right ends of the timeline axis in the mock's 1000-unit
/// viewBox. `x` in 0..1 maps linearly onto them.
const AXIS_LEFT: f64 = 78.0;
const AXIS_SPAN: f64 = 902.0;
/// The three lanes: decisions above the axis, commits on it, sessions below.
const LANE_DECISION: i32 = 42;
const LANE_COMMIT: i32 = 76;
const LANE_SESSION: i32 = 110;
/// Beyond this many day ticks the axis is thinned rather than crowded.
const MAX_TICKS: usize = 24;

static TERA: OnceLock<std::result::Result<Tera, String>> = OnceLock::new();

/// The compiled template, built once per process.
pub fn tera() -> Result<&'static Tera> {
    TERA.get_or_init(build)
        .as_ref()
        .map_err(|e| anyhow!("the project page template does not compile: {e}"))
}

fn build() -> std::result::Result<Tera, String> {
    let mut tera = Tera::default();
    // The template is HTML; everything interpolated into it is escaped.
    // Tera's own escaper also turns `/` into an entity, which would make every
    // URL on the page unreadable, so the page uses the five-character escape.
    tera.autoescape_on(vec![".tera", ".html"]);
    tera.set_escape_fn(escape);
    tera.register_filter("day", f_day);
    tera.register_filter("day_month", f_day_month);
    tera.register_filter("daytime", f_daytime);
    tera.register_filter("stamp", f_stamp);
    tera.register_filter("num", f_num);
    tera.register_filter("cut", f_cut);
    tera.register_filter("linkname", f_linkname);
    tera.register_filter("kindlabel", f_kindlabel);
    tera.register_filter("countword", f_countword);
    tera.register_filter("modelname", f_modelname);
    tera.register_filter("cx", f_cx);
    tera.register_filter("cy", f_cy);
    tera.register_filter("diamond", f_diamond);
    tera.register_filter("diffhtml", f_diffhtml);
    tera.add_raw_template(NAME, TEMPLATE)
        .map_err(|e| format!("{e:#}"))?;
    Ok(tera)
}

/// Render `ctx` plus the values that are derived from it rather than recorded.
pub fn render(ctx: &PageContext) -> Result<String> {
    let mut context = Context::from_serialize(ctx)?;
    context.insert(
        "ticks",
        &day_ticks(
            ctx.project.first_ts.as_deref(),
            ctx.project.last_ts.as_deref(),
        ),
    );
    context.insert("ref_labels", &super::ref_labels(ctx));
    context.insert("ratio_view", &ctx.ratio.map(RatioView::of));
    context.insert("verdict", &Verdict::of(ctx));
    let html = tera()?.render(NAME, &context)?;
    Ok(html)
}

// ---------------------------------------------------------------------------
// derived values
// ---------------------------------------------------------------------------

/// The percentages under the attribution bar. They are worked out here so the
/// three of them always add up to a hundred.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct RatioView {
    pub total: u64,
    pub accepted_pct: u64,
    pub reworked_pct: u64,
    pub human_pct: u64,
    /// Lines that came out of an AI session, kept or reworked: the one number
    /// the first screen leads with.
    pub ai_pct: u64,
}

impl RatioView {
    pub fn of(ratio: super::Ratio) -> RatioView {
        let total = ratio.ai_lines + ratio.human_lines;
        if total == 0 {
            return RatioView {
                total: 0,
                accepted_pct: 0,
                reworked_pct: 0,
                human_pct: 0,
                ai_pct: 0,
            };
        }
        let pct = |n: u64| ((n as f64 * 100.0) / total as f64).round() as u64;
        let accepted_pct = pct(ratio.accepted_unchanged);
        let reworked_pct = pct(ratio.human_reworked).min(100 - accepted_pct);
        RatioView {
            total,
            accepted_pct,
            reworked_pct,
            human_pct: 100 - accepted_pct - reworked_pct,
            ai_pct: accepted_pct + reworked_pct,
        }
    }
}

/// The first screen, for a reader who gives the page six seconds: one line
/// saying what was built, the span of the work, and up to three plain facts
/// about how it was done. All of it is derived from the context, so a fact
/// the log does not support cannot appear here.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Verdict {
    /// The intent, cut down to one headline.
    pub headline: String,
    /// True when the headline is a shortened form and the full intent is
    /// worth printing underneath it.
    pub headline_is_cut: bool,
    /// `17 Sep – 21 Sep`, `3 Sep` for a one-day project, empty for an empty log.
    pub span: String,
    pub highlights: Vec<String>,
}

/// A headline longer than this is cut at a word boundary.
const HEADLINE_MAX: usize = 72;
/// A quoted decision longer than this is cut at a word boundary.
const DECISION_MAX: usize = 96;

impl Verdict {
    pub fn of(ctx: &PageContext) -> Verdict {
        let headline = headline(&ctx.project.name, &ctx.project.what);
        // A full stop or a capital is not a cut; only lost words are.
        let full = ctx.project.what.trim().trim_end_matches(['.', ',']);
        Verdict {
            headline_is_cut: !full.is_empty() && !full.eq_ignore_ascii_case(&headline),
            headline,
            span: span(
                ctx.project.first_ts.as_deref(),
                ctx.project.last_ts.as_deref(),
            ),
            highlights: highlights(ctx),
        }
    }
}

/// The intent as a headline: without the project's own name in front, cut
/// at the first colon or semicolon, capped at [`HEADLINE_MAX`] characters,
/// and starting with a capital. `How <name> came together` when there is no
/// intent at all.
pub fn headline(name: &str, what: &str) -> String {
    let mut text = what.trim();
    if text.is_empty() {
        let name = name.trim();
        return if name.is_empty() {
            "How this project came together".to_string()
        } else {
            format!("How {name} came together")
        };
    }

    // "madelog, a CLI that …" → "a CLI that …"
    let name = name.trim();
    if !name.is_empty()
        && text.len() > name.len()
        && text.is_char_boundary(name.len())
        && text[..name.len()].eq_ignore_ascii_case(name)
    {
        let rest = &text[name.len()..];
        for sep in [", ", ": ", " - ", " – ", " — ", " is "] {
            if let Some(after) = rest.strip_prefix(sep) {
                text = after.trim_start();
                break;
            }
        }
    }

    // The first clause is the headline; what follows a colon is the list.
    for sep in [':', ';'] {
        if let Some(i) = text.find(sep) {
            text = text[..i].trim_end();
        }
    }
    for sep in [" — ", " – "] {
        if let Some(i) = text.find(sep) {
            text = text[..i].trim_end();
        }
    }

    let mut out = cut_words(text, HEADLINE_MAX);
    if let Some(first) = out.chars().next() {
        if first.is_lowercase() {
            let upper: String = first.to_uppercase().collect();
            out.replace_range(..first.len_utf8(), &upper);
        }
    }
    out.trim_end_matches(['.', ',']).to_string()
}

/// `text` if it fits in `max` characters, else the longest run of whole
/// words that does, with an ellipsis.
fn cut_words(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    // Byte offset of the last space inside the first `max` characters.
    let mut end = 0;
    for (n, (i, ch)) in text.char_indices().enumerate() {
        if n >= max {
            break;
        }
        if ch == ' ' {
            end = i;
        }
    }
    if end == 0 {
        end = text
            .char_indices()
            .nth(max)
            .map(|(i, _)| i)
            .unwrap_or(text.len());
    }
    format!("{}…", text[..end].trim_end_matches(['.', ',', ';', ':']))
}

/// `17 Sep – 21 Sep`, or a single day when the work fits in one.
fn span(first: Option<&str>, last: Option<&str>) -> String {
    match (first, last) {
        (Some(a), Some(b)) => {
            let (a, b) = (day_month(a), day_month(b));
            if a == b {
                a
            } else {
                format!("{a} – {b}")
            }
        }
        (Some(a), None) | (None, Some(a)) => day_month(a),
        (None, None) => String::new(),
    }
}

/// Up to three one-line facts, in the order a reader wants them: how the AI
/// share was handled, the last decision written down, and that the record
/// was kept as it happened.
fn highlights(ctx: &PageContext) -> Vec<String> {
    let mut out = Vec::new();

    if let Some(ratio) = ctx.ratio {
        if ratio.ai_lines > 0 {
            out.push(if ratio.human_reworked > 0 {
                format!(
                    "Reworked {} of {} AI-written lines by hand",
                    num(ratio.human_reworked),
                    num(ratio.ai_lines)
                )
            } else {
                format!(
                    "Kept all {} AI-written lines as they came",
                    num(ratio.ai_lines)
                )
            });
        }
    }

    if let Some(decision) = ctx
        .timeline
        .iter()
        .rev()
        .find(|e| e.kind == "decision")
        .map(|e| e.title.trim())
        .filter(|t| !t.is_empty())
    {
        out.push(format!("Decided: {}", cut_words(decision, DECISION_MAX)));
    }

    if ctx.stats.commits > 0 {
        let mut line = format!(
            "{} commit{} recorded as {} happened, {} event{} hash-chained",
            ctx.stats.commits,
            plural(ctx.stats.commits),
            if ctx.stats.commits == 1 { "it" } else { "they" },
            num(ctx.chained_events),
            plural(ctx.chained_events)
        );
        if ctx.badge.published.is_some() {
            line.push_str(", root confirmed by the server");
        }
        out.push(line);
    }

    out.truncate(3);
    out
}

fn plural(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// `1310` → `1,310`, the way the `num` filter prints it.
fn num(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// One day mark under the axis.
#[derive(Debug, Clone, Serialize)]
pub struct Tick {
    pub cx: String,
    pub label: String,
    /// Mondays are drawn heavier and always carry the month.
    pub major: bool,
}

/// A tick for every midnight between the first and the last event, thinned
/// when a project ran long enough to crowd the axis.
fn day_ticks(first: Option<&str>, last: Option<&str>) -> Vec<Tick> {
    let (Some(first), Some(last)) = (first.and_then(parse), last.and_then(parse)) else {
        return Vec::new();
    };
    let millis = (last - first).num_milliseconds();
    if millis <= 0 {
        return Vec::new();
    }

    let mut days: Vec<DateTime<Utc>> = Vec::new();
    let mut day = first
        .date_naive()
        .succ_opt()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc());
    while let Some(at) = day {
        if at > last {
            break;
        }
        days.push(at);
        day = at
            .date_naive()
            .succ_opt()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|d| d.and_utc());
    }

    let step = days.len().div_ceil(MAX_TICKS).max(1);
    days.iter()
        .step_by(step)
        .enumerate()
        .map(|(i, at)| {
            let major = at.weekday() == Weekday::Mon;
            let x = (*at - first).num_milliseconds() as f64 / millis as f64;
            Tick {
                cx: format!("{:.1}", AXIS_LEFT + x * AXIS_SPAN),
                label: if major || i == 0 {
                    at.format("%-d %b").to_string()
                } else {
                    at.format("%-d").to_string()
                },
                major,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// filters
// ---------------------------------------------------------------------------

/// `2026-09-03T09:12:00Z` → `3 Sep 2026`.
pub fn day(ts: &str) -> String {
    fmt(ts, "%-d %b %Y")
}

/// `2026-09-03T09:12:00Z` → `3 Sep`.
pub fn day_month(ts: &str) -> String {
    fmt(ts, "%-d %b")
}

/// The name a kind goes under on the page.
pub fn kind_label(kind: &str) -> &str {
    match kind {
        "ai_session" => "AI session",
        "hidden" => "hidden session",
        other => other,
    }
}

fn fmt(ts: &str, pattern: &str) -> String {
    parse(ts)
        .map(|t| t.format(pattern).to_string())
        .unwrap_or_default()
}

/// RFC 3339, or a bare `YYYY-MM-DD` as the weekly summaries carry.
fn parse(ts: &str) -> Option<DateTime<Utc>> {
    if let Ok(t) = DateTime::parse_from_rfc3339(ts) {
        return Some(t.with_timezone(&Utc));
    }
    NaiveDate::parse_from_str(ts, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc())
}

fn as_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn as_u64(value: &Value) -> u64 {
    value
        .as_f64()
        .map(|n| n.max(0.0).round() as u64)
        .unwrap_or(0)
}

fn as_f64(value: &Value) -> f64 {
    value.as_f64().unwrap_or(0.0)
}

fn text(s: String) -> tera::Result<Value> {
    Ok(Value::String(s))
}

fn f_day(v: &Value, _: &HashMap<String, Value>) -> tera::Result<Value> {
    text(day(&as_str(v)))
}

fn f_day_month(v: &Value, _: &HashMap<String, Value>) -> tera::Result<Value> {
    text(day_month(&as_str(v)))
}

/// `2026-09-03T09:35:00Z` → `3 Sep, 09:35`.
fn f_daytime(v: &Value, _: &HashMap<String, Value>) -> tera::Result<Value> {
    text(fmt(&as_str(v), "%-d %b, %H:%M"))
}

/// `2026-09-03T09:12:00Z` → `3 Sep 2026, 09:12 UTC`.
fn f_stamp(v: &Value, _: &HashMap<String, Value>) -> tera::Result<Value> {
    text(fmt(&as_str(v), "%-d %b %Y, %H:%M UTC"))
}

/// `1310` → `1,310`.
fn f_num(v: &Value, _: &HashMap<String, Value>) -> tera::Result<Value> {
    let digits = as_u64(v).to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    text(out)
}

/// The first `len` characters, twelve by default. Used for the short root.
fn f_cut(v: &Value, args: &HashMap<String, Value>) -> tera::Result<Value> {
    let s = as_str(v);
    let len = args.get("len").map(as_u64).unwrap_or(12) as usize;
    text(s.chars().take(len).collect())
}

/// `https://github.com/x/y/` → `github.com/x/y`, for link text.
fn f_linkname(v: &Value, _: &HashMap<String, Value>) -> tera::Result<Value> {
    let s = as_str(v);
    let s = s
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    text(s.to_string())
}

fn f_kindlabel(v: &Value, _: &HashMap<String, Value>) -> tera::Result<Value> {
    text(kind_label(&as_str(v)).to_string())
}

/// `4` → `Four`, so a heading can say how many moments there are.
fn f_countword(v: &Value, _: &HashMap<String, Value>) -> tera::Result<Value> {
    let n = as_u64(v);
    text(match n {
        0 => "No".to_string(),
        1 => "One".to_string(),
        2 => "Two".to_string(),
        3 => "Three".to_string(),
        4 => "Four".to_string(),
        5 => "Five".to_string(),
        other => other.to_string(),
    })
}

/// `ollama:qwen3:4b` → `qwen3:4b`.
fn f_modelname(v: &Value, _: &HashMap<String, Value>) -> tera::Result<Value> {
    let s = as_str(v);
    text(match s.split_once(':') {
        Some((_, model)) if !model.is_empty() => model.to_string(),
        _ => s,
    })
}

/// `x` in 0..1 → the `cx` of a point on the axis.
fn f_cx(v: &Value, _: &HashMap<String, Value>) -> tera::Result<Value> {
    text(format!("{:.1}", AXIS_LEFT + as_f64(v) * AXIS_SPAN))
}

/// The lane a kind sits in.
fn f_cy(v: &Value, _: &HashMap<String, Value>) -> tera::Result<Value> {
    let lane = match as_str(v).as_str() {
        "decision" => LANE_DECISION,
        "ai_session" | "hidden" => LANE_SESSION,
        _ => LANE_COMMIT,
    };
    Ok(Value::from(lane))
}

/// The diamond a decision is drawn as, as an SVG path.
fn f_diamond(v: &Value, _: &HashMap<String, Value>) -> tera::Result<Value> {
    let cx = AXIS_LEFT + as_f64(v) * AXIS_SPAN;
    let (top, bottom) = (LANE_DECISION - 6, LANE_DECISION + 6);
    text(format!(
        "M{cx:.1} {top}L{:.1} {LANE_DECISION}L{cx:.1} {bottom}L{:.1} {LANE_DECISION}Z",
        cx + 6.0,
        cx - 6.0
    ))
}

/// A diff excerpt as marked-up, already-escaped HTML.
///
/// The filter escapes its own input because its output is used with `| safe`;
/// nothing else in the template is.
fn f_diffhtml(v: &Value, _: &HashMap<String, Value>) -> tera::Result<Value> {
    let raw = as_str(v);
    let mut out = String::with_capacity(raw.len() * 2);
    for (i, line) in raw.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let class = match line.as_bytes().first() {
            Some(b'+') => "add",
            Some(b'-') => "del",
            _ => "ctx",
        };
        out.push_str("<span class=\"");
        out.push_str(class);
        out.push_str("\">");
        out.push_str(&escape(line));
        out.push_str("</span>");
    }
    text(out)
}

fn escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(f: fn(&Value, &HashMap<String, Value>) -> tera::Result<Value>, v: Value) -> String {
        as_str(&f(&v, &HashMap::new()).unwrap())
    }

    #[test]
    fn dates_read_the_way_the_mock_writes_them() {
        let ts = Value::from("2026-09-03T09:35:00Z");
        assert_eq!(call(f_day, ts.clone()), "3 Sep 2026");
        assert_eq!(call(f_daytime, ts.clone()), "3 Sep, 09:35");
        assert_eq!(call(f_stamp, ts), "3 Sep 2026, 09:35 UTC");
    }

    #[test]
    fn numbers_get_thousands_separators() {
        assert_eq!(call(f_num, Value::from(1310)), "1,310");
        assert_eq!(call(f_num, Value::from(999)), "999");
        assert_eq!(call(f_num, Value::from(1000000)), "1,000,000");
        assert_eq!(call(f_num, Value::from(0)), "0");
    }

    #[test]
    fn x_maps_onto_the_mocks_axis() {
        assert_eq!(call(f_cx, Value::from(0.1676)), "229.2");
        assert_eq!(call(f_cx, Value::from(0.0)), "78.0");
        assert_eq!(call(f_cx, Value::from(1.0)), "980.0");
        assert_eq!(
            call(f_diamond, Value::from(0.0063)),
            "M83.7 36L89.7 42L83.7 48L77.7 42Z"
        );
    }

    #[test]
    fn a_diff_excerpt_is_escaped_by_the_filter() {
        let out = call(f_diffhtml, Value::from("+let x = \"<b>\";\n-gone\n ctx"));
        assert!(out.contains("<span class=\"add\">+let x = &quot;&lt;b&gt;&quot;;</span>"));
        assert!(out.contains("<span class=\"del\">-gone</span>"));
        assert!(out.contains("<span class=\"ctx\"> ctx</span>"));
    }

    #[test]
    fn percentages_add_up_to_a_hundred() {
        let view = RatioView::of(super::super::Ratio {
            ai_lines: 2140,
            accepted_unchanged: 1310,
            human_reworked: 830,
            human_lines: 3420,
        });
        assert_eq!(view.total, 5560);
        assert_eq!(view.accepted_pct, 24);
        assert_eq!(view.reworked_pct, 15);
        assert_eq!(view.human_pct, 61);
        assert_eq!(view.ai_pct, 39, "kept plus reworked is the AI share");
    }

    #[test]
    fn the_headline_is_the_first_clause_without_the_name() {
        assert_eq!(
            headline(
                "madelog",
                "madelog, a CLI that records how a project came together: commits, AI \
                 sessions and one-sentence decisions, hash chained, rendered as a page"
            ),
            "A CLI that records how a project came together"
        );
        assert_eq!(
            headline(
                "ledger",
                "Ledger is a bookkeeping tool for freelancers; nothing else."
            ),
            "A bookkeeping tool for freelancers"
        );
        assert_eq!(
            headline("x", "a small cli that records process"),
            "A small cli that records process"
        );
        // Long single clause: cut at a word, with an ellipsis.
        let long = "an internal dashboard that shows every open ticket across all four teams \
                    and the person responsible for it";
        let cut = headline("dash", long);
        assert!(cut.ends_with('…'), "{cut}");
        assert!(cut.chars().count() <= HEADLINE_MAX + 1, "{cut}");
        assert!(!cut.contains("responsible"), "{cut}");
        // No intent at all.
        assert_eq!(headline("madelog", ""), "How madelog came together");
        assert_eq!(headline("", "  "), "How this project came together");
        // A name that is not at the front is left alone.
        assert_eq!(
            headline("madelog", "A page about madelog"),
            "A page about madelog"
        );
    }

    #[test]
    fn the_span_is_two_days_or_one() {
        assert_eq!(
            span(Some("2026-09-17T20:00:00Z"), Some("2026-09-21T08:00:00Z")),
            "17 Sep – 21 Sep"
        );
        assert_eq!(
            span(Some("2026-09-17T08:00:00Z"), Some("2026-09-17T20:00:00Z")),
            "17 Sep"
        );
        assert_eq!(span(None, None), "");
    }

    #[test]
    fn highlights_only_say_what_the_log_supports() {
        use super::super::{Badge, Published, Ratio, Stats, TimelineEntry};

        let mut ctx = PageContext::default();
        assert!(
            Verdict::of(&ctx).highlights.is_empty(),
            "an empty log has nothing to say"
        );

        ctx.stats = Stats {
            commits: 40,
            decisions: 9,
            ai_sessions: 20,
            days_active: 2,
        };
        ctx.chained_events = 87;
        ctx.ratio = Some(Ratio {
            ai_lines: 2140,
            accepted_unchanged: 1310,
            human_reworked: 830,
            human_lines: 3420,
        });
        ctx.timeline = vec![
            TimelineEntry {
                kind: "decision".into(),
                title: "Server in Go, not Rust".into(),
                ..Default::default()
            },
            TimelineEntry {
                kind: "commit".into(),
                title: "feat: later".into(),
                ..Default::default()
            },
        ];
        let v = Verdict::of(&ctx);
        assert_eq!(
            v.highlights,
            vec![
                "Reworked 830 of 2,140 AI-written lines by hand",
                "Decided: Server in Go, not Rust",
                "40 commits recorded as they happened, 87 events hash-chained",
            ]
        );

        ctx.badge = Badge {
            published: Some(Published {
                slug: "s".into(),
                url: "u".into(),
                received_at: "2026-09-21T08:00:00Z".into(),
                verify_url: "v".into(),
            }),
            ..Default::default()
        };
        ctx.ratio = Some(Ratio {
            ai_lines: 12,
            accepted_unchanged: 12,
            human_reworked: 0,
            human_lines: 0,
        });
        ctx.stats.commits = 1;
        let v = Verdict::of(&ctx);
        assert_eq!(v.highlights[0], "Kept all 12 AI-written lines as they came");
        assert_eq!(
            v.highlights[2],
            "1 commit recorded as it happened, 87 events hash-chained, root confirmed by the server"
        );

        // A very long decision is cut at a word.
        ctx.timeline[0].title = "Chose ".to_string() + &"word ".repeat(40);
        let v = Verdict::of(&ctx);
        assert!(v.highlights[1].ends_with('…'));
        assert!(v.highlights[1].chars().count() <= "Decided: ".len() + DECISION_MAX + 1);
    }

    #[test]
    fn day_ticks_land_where_the_mock_put_them() {
        let ticks = day_ticks(Some("2026-09-03T09:12:00Z"), Some("2026-09-17T14:02:00Z"));
        assert_eq!(ticks.len(), 14);
        assert_eq!(ticks[0].cx, "117.2");
        assert_eq!(ticks[0].label, "4 Sep");
        assert!(!ticks[0].major);
        assert_eq!(ticks[3].label, "7 Sep");
        assert!(ticks[3].major, "7 Sep 2026 is a Monday");
        assert_eq!(ticks[13].cx, "942.9");
    }

    #[test]
    fn a_long_project_does_not_crowd_the_axis() {
        let ticks = day_ticks(Some("2024-01-01T00:00:00Z"), Some("2024-12-31T00:00:00Z"));
        assert!(ticks.len() <= MAX_TICKS, "{} ticks", ticks.len());
        assert!(!ticks.is_empty());
    }

    #[test]
    fn model_names_come_out_of_their_provider() {
        assert_eq!(
            call(f_modelname, Value::from("ollama:qwen3:4b")),
            "qwen3:4b"
        );
        assert_eq!(
            call(f_modelname, Value::from("remote:claude-sonnet-4-5")),
            "claude-sonnet-4-5"
        );
        assert_eq!(call(f_modelname, Value::from("heuristic")), "heuristic");
    }
}
