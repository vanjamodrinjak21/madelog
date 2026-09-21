//! Everything the post-commit hook does once the commit itself is recorded.
//!
//! Pattern analysis, weekly summaries and the page, in that order, so that by
//! the time you look at `.madelog/site/index.html` it already knows about the
//! commit you just made. Nothing here asks a question, nothing here prints to
//! stdout, and nothing here is allowed to be slow: heavy work is skipped when
//! there is nothing new to say, and a local model gets three seconds before
//! the template takes over.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;

use crate::config::Config;
use crate::event::{EventKind, PatternPayload, Payload, SummaryPayload};
use crate::store::Store;
use crate::summarize::{self, Week};

/// What a refresh actually did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Refreshed {
    pub patterns: usize,
    pub summaries: usize,
    pub rendered: bool,
}

impl Refreshed {
    pub fn changed(&self) -> bool {
        self.patterns > 0 || self.summaries > 0
    }
}

/// Run the whole thing.
///
/// `already_changed` says whether the caller appended anything — a commit, an
/// AI session — so that a hook that had nothing to record does not rewrite a
/// page that would come out byte for byte the same.
pub fn refresh(
    root: &Path,
    store: &Store,
    config: &Config,
    already_changed: bool,
) -> Result<Refreshed> {
    let mut done = Refreshed {
        patterns: analyse(store)?,
        ..Refreshed::default()
    };
    done.summaries = summarise(root, store, config)?;

    let page = root.join(".madelog").join("site").join("index.html");
    if already_changed || done.changed() || !page.exists() {
        crate::page::render_local(root, published(config))?;
        done.rendered = true;
    }
    Ok(done)
}

/// The publication facts the page badge shows, when this repository has a
/// page online. A local render of a published repository should not quietly
/// demote itself to "not yet published".
pub fn published(config: &Config) -> Option<crate::page::Published> {
    let slug = config
        .server
        .slug
        .clone()
        .filter(|s| !s.trim().is_empty())?;
    let base = crate::publish::normalise_base(&config.server.url);
    Some(crate::page::Published {
        url: format!("{base}/p/{slug}"),
        verify_url: format!("{base}/v1/verify/{slug}"),
        received_at: config.server.published_at.clone().unwrap_or_default(),
        slug,
    })
}

/// Append a `pattern` event for anything new. Patterns already in the log are
/// left alone, so this is free to run after every commit.
pub fn analyse(store: &Store) -> Result<usize> {
    let events = store.all()?;
    let found = crate::patterns::detect(&events);
    let already: BTreeSet<String> = events
        .iter()
        .filter(|e| e.kind == EventKind::Pattern)
        .filter_map(|e| e.payload_as::<PatternPayload>().ok())
        .map(|p| fingerprint(&p))
        .collect();

    let mut added = 0;
    for pattern in &found {
        if already.contains(&fingerprint(pattern)) {
            continue;
        }
        store.append_payload(anchor(pattern), &Payload::Pattern(pattern.clone()))?;
        added += 1;
    }
    Ok(added)
}

/// What makes two patterns the same: the rule and the commits it covers.
fn fingerprint(pattern: &PatternPayload) -> String {
    let mut commits: Vec<&str> = pattern.commits.iter().map(String::as_str).collect();
    commits.sort_unstable();
    format!(
        "{} {}",
        crate::patterns::kind_str(pattern.pattern),
        commits.join(" ")
    )
}

fn anchor(pattern: &PatternPayload) -> Option<&str> {
    pattern
        .commits
        .last()
        .map(String::as_str)
        .filter(|sha| sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Keep the weekly summaries current.
///
/// A week with no summary gets one. The newest week gets a fresh one whenever
/// the facts have moved on — that is the week you are still working in — but
/// never when you have edited it yourself. Older weeks are finished, and
/// rewriting them every commit would fill the log with noise.
pub fn summarise(root: &Path, store: &Store, config: &Config) -> Result<usize> {
    // Commits that git's history no longer has are set aside here exactly
    // as the page sets them aside, so a summary and the page agree.
    let (events, _) = crate::page::without_rewritten(store.all()?, root);
    let weeks = summarize::bucket(&events);
    if weeks.is_empty() {
        return Ok(0);
    }
    let latest = summarize::latest_summaries(&events);
    let newest_week = weeks.last().map(|w| w.week_start());

    let mut todo: Vec<&Week> = Vec::new();
    for week in &weeks {
        let start = week.week_start();
        match latest.iter().find(|s| s.week_start == start) {
            None => todo.push(week),
            Some(existing) => {
                if Some(&start) == newest_week.as_ref() && !existing.edited {
                    todo.push(week);
                }
            }
        }
    }
    if todo.is_empty() {
        return Ok(0);
    }

    let provider = hook_provider(config);
    let mut written = 0;
    for week in todo {
        let text = provider.write(week);
        let start = week.week_start();
        // The whole point of rewriting the newest week is that it says
        // something new; an identical summary is not worth a chain entry.
        if let Some(existing) = latest.iter().find(|s| s.week_start == start) {
            if existing.text.trim() == text.text.trim()
                && existing.generated_by == text.generated_by
            {
                continue;
            }
        }
        store.append_payload(
            None,
            &Payload::Summary(SummaryPayload {
                week_start: start,
                week_end: week.week_end(),
                text: text.text,
                generated_by: text.generated_by,
                edited: false,
            }),
        )?;
        written += 1;
    }
    Ok(written)
}

/// Which provider a hook is allowed to use.
///
/// Only a model that is already on this machine: `llm.provider = ollama`, or
/// `auto` with the configured model pulled. Anything else is the template,
/// because a hook is not the place to discover that Ollama is not running.
/// `madelog summarize` upgrades the week later, on purpose and with a wait
/// the person chose.
pub fn hook_provider(config: &Config) -> summarize::Provider {
    if config.llm.remote {
        return summarize::Provider::Heuristic;
    }
    let model = config.llm.model.trim();
    if model.is_empty() || !matches!(config.llm.provider.trim(), "ollama" | "auto") {
        return summarize::Provider::Heuristic;
    }
    let host = summarize::ollama::host(config);
    let client = summarize::Ollama::impatient(&host);
    if !client.has_model(model) {
        return summarize::Provider::Heuristic;
    }
    summarize::Provider::Ollama {
        model: model.to_string(),
        client: Box::new(client),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hook_never_reaches_for_a_model_that_is_not_there() {
        let mut config = Config::default();
        // Nothing is listening on this port, so `auto` must fall back.
        config.llm.host = Some("http://127.0.0.1:1".to_string());
        assert!(matches!(
            hook_provider(&config),
            summarize::Provider::Heuristic
        ));

        config.llm.provider = "none".into();
        assert!(matches!(
            hook_provider(&config),
            summarize::Provider::Heuristic
        ));

        config.llm.provider = "ollama".into();
        assert!(
            matches!(hook_provider(&config), summarize::Provider::Heuristic),
            "even `ollama` is not worth hanging a commit on"
        );

        config.llm.provider = "auto".into();
        config.llm.remote = true;
        assert!(matches!(
            hook_provider(&config),
            summarize::Provider::Heuristic
        ));
    }

    #[test]
    fn nothing_changed_means_nothing_changed() {
        assert!(!Refreshed::default().changed());
        assert!(Refreshed {
            patterns: 1,
            ..Refreshed::default()
        }
        .changed());
        assert!(Refreshed {
            summaries: 1,
            ..Refreshed::default()
        }
        .changed());
        assert!(
            !Refreshed {
                rendered: true,
                ..Refreshed::default()
            }
            .changed(),
            "a render is a consequence, not a change to the log"
        );
    }

    #[test]
    fn a_published_repository_keeps_its_badge_on_a_local_render() {
        let mut config = Config::default();
        assert!(published(&config).is_none());

        config.server.url = "https://madelog.app".into();
        config.server.slug = Some("quiet-fern-4821".into());
        config.server.published_at = Some("2026-09-18T10:00:00Z".into());
        let p = published(&config).unwrap();
        assert_eq!(p.url, "https://madelog.app/p/quiet-fern-4821");
        assert_eq!(
            p.verify_url,
            "https://madelog.app/v1/verify/quiet-fern-4821"
        );
        assert_eq!(p.received_at, "2026-09-18T10:00:00Z");
    }
}
