//! `madelog status` — one screen that answers "is this thing on?".
//!
//! Everything here is read-only and local except two short probes: Ollama's
//! model list and nothing else. The server is not contacted; `madelog doctor`
//! is the command that reaches out.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::commands::{backfill, init};
use crate::config::Config;
use crate::event::{CommitPayload, EventKind};
use crate::gitrepo::Repo;
use crate::store::Store;
use crate::Ctx;

/// Everything `madelog status --json` prints, and everything the plain form
/// reads from.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub repo: String,
    pub events: u64,
    /// Event counts by kind, e.g. `{"commit": 380, "decision": 12}`.
    pub events_by_kind: BTreeMap<String, u64>,
    pub last_commit: Option<LastCommit>,
    /// Commits on the first-parent line from HEAD that the log does not have.
    pub unrecorded_commits: u64,
    pub hooks: Vec<Hook>,
    /// Every AI tool madelog knows how to read, and what it found here.
    pub ai_tools: Vec<AiTool>,
    /// Whether the AI sessions behind your commits are being recorded, and
    /// why not when they are not.
    pub ai_sessions: String,
    pub ollama: OllamaStatus,
    pub server_url: String,
    pub published: Option<Publication>,
    pub page: PageStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct LastCommit {
    pub sha: String,
    /// When madelog recorded it.
    pub recorded_at: String,
    /// When git says it was made; absent for commits recorded before madelog
    /// carried the field.
    pub committed_at: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hook {
    pub name: String,
    pub installed: bool,
    /// The madelog the hook calls, as its line names it.
    pub command: Option<String>,
    /// Whether that command exists and can be run.
    pub command_exists: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiTool {
    pub name: String,
    pub installed: bool,
    pub note: String,
    /// Sessions touching this repository in the last three months.
    pub sessions: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OllamaStatus {
    /// `auto | ollama | none`, from `llm.provider`.
    pub provider: String,
    pub host: String,
    pub reachable: bool,
    pub model: String,
    /// Whether that model has been pulled. `false` when Ollama is unreachable.
    pub model_ready: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Publication {
    pub slug: String,
    pub url: String,
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageStatus {
    pub path: String,
    pub rendered: bool,
    /// `true` when the rendered page is newer than the last publish, so the
    /// link points at something older than what is on disk.
    pub newer_than_publish: bool,
    /// `true` when the log has moved on since the page was rendered, so the
    /// file on disk is out of date. `madelog open` re-renders when it is.
    pub older_than_log: bool,
}

pub fn run(ctx: &mut Ctx<'_>, json: bool) -> Result<()> {
    let status = read(&ctx.cwd)?;
    if json {
        let body = serde_json::to_string_pretty(&status).context("rendering the status as JSON")?;
        ctx.say(&body)?;
        return Ok(());
    }
    print(ctx, &status)
}

/// Gather the status of the repository containing `cwd`.
pub fn read(cwd: &Path) -> Result<Status> {
    let repo = Repo::discover(cwd)?;
    let root = repo.workdir()?;
    let madelog_dir = root.join(".madelog");
    let config = Config::load(&madelog_dir.join("config.toml"))?;

    let db = madelog_dir.join("log.db");
    let (events, events_by_kind, last_commit, unrecorded_commits) = if db.exists() {
        let store = Store::open(&db)?;
        let all = store.all()?;
        let mut by_kind: BTreeMap<String, u64> = BTreeMap::new();
        for event in &all {
            *by_kind.entry(event.kind.as_str().to_string()).or_default() += 1;
        }
        let last = store.last_of_kind(EventKind::Commit)?.map(|event| {
            let payload = event.payload_as::<CommitPayload>().ok();
            LastCommit {
                sha: event.commit_sha.clone().unwrap_or_default(),
                recorded_at: event.ts.clone(),
                committed_at: payload.as_ref().and_then(|p| p.committed_at.clone()),
                message: payload
                    .as_ref()
                    .map(|p| first_line(&p.message))
                    .unwrap_or_default(),
            }
        });
        let missing = backfill::unrecorded(&repo, &store)?.len() as u64;
        (all.len() as u64, by_kind, last, missing)
    } else {
        (0, BTreeMap::new(), None, 0)
    };

    let git_dir = repo.git_dir();
    let hooks = init::HOOKS
        .iter()
        .map(|(name, _)| {
            let command = init::installed_hook_command(&git_dir, name);
            Hook {
                name: (*name).to_string(),
                installed: init::hook_is_installed(&git_dir, name),
                command_exists: command.as_deref().is_some_and(command_exists),
                command,
            }
        })
        .collect();

    let page_path = madelog_dir.join("site").join("index.html");
    let rendered = page_path.exists();
    let older_than_log = rendered && crate::commands::open::page_is_stale(&root);
    let published_at = config
        .server
        .published_at
        .clone()
        .filter(|v| !v.trim().is_empty());
    let published = config
        .server
        .slug
        .clone()
        .filter(|s| !s.trim().is_empty())
        .map(|slug| Publication {
            url: page_url(&config.server.url, &slug),
            slug,
            published_at: published_at.clone(),
        });

    Ok(Status {
        repo: root.display().to_string(),
        events,
        events_by_kind,
        last_commit,
        unrecorded_commits,
        hooks,
        ai_tools: crate::capture::report(Some(&root))
            .into_iter()
            .map(|r| AiTool {
                name: r.name,
                installed: r.installed,
                note: r.note,
                sessions: r.sessions,
            })
            .collect(),
        ai_sessions: crate::commands::hook::ai_session_state(&root).1,
        ollama: ollama_status(&config),
        server_url: config.server.url.clone(),
        page: PageStatus {
            newer_than_publish: rendered
                && published.is_some()
                && is_newer(&page_path, published_at.as_deref()),
            older_than_log,
            path: page_path.display().to_string(),
            rendered,
        },
        published,
    })
}

fn ollama_status(config: &Config) -> OllamaStatus {
    let host = crate::summarize::ollama::host(config);
    let model = config.llm.model.clone();
    if config.llm.provider == "none" {
        return OllamaStatus {
            provider: config.llm.provider.clone(),
            host,
            reachable: false,
            model,
            model_ready: false,
        };
    }
    let client = crate::summarize::Ollama::new(&host);
    // One request answers both questions; asking twice doubles the wait for a
    // host that is not there.
    let answer = client.models();
    let reachable = answer.is_ok();
    let models = answer.unwrap_or_default();
    OllamaStatus {
        provider: config.llm.provider.clone(),
        host,
        model_ready: reachable && models.iter().any(|have| same_model(have, &model)),
        model,
        reachable,
    }
}

/// `qwen3:4b` is also `qwen3:4b:latest`, and a bare name is its `:latest` tag.
fn same_model(have: &str, want: &str) -> bool {
    let strip = |s: &str| s.trim().trim_end_matches(":latest").to_string();
    strip(have) == strip(want)
}

/// Is the file at `path` newer than `since`?
fn is_newer(path: &Path, since: Option<&str>) -> bool {
    let Some(since) = since.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) else {
        // Published, but madelog never wrote down when — an older config. The
        // honest answer is "no idea", and "no idea" must not nag.
        return false;
    };
    let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) else {
        return false;
    };
    chrono::DateTime::<chrono::Utc>::from(modified) > since.with_timezone(&chrono::Utc)
}

fn command_exists(command: &str) -> bool {
    if command.contains('/') || command.contains('\\') {
        return Path::new(command).is_file();
    }
    init::which(command).is_some()
}

fn page_url(server_url: &str, slug: &str) -> String {
    format!("{}/p/{slug}", crate::publish::normalise_base(server_url))
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

// ---------------------------------------------------------------------------
// the screen
// ---------------------------------------------------------------------------

fn print(ctx: &mut Ctx<'_>, s: &Status) -> Result<()> {
    line(ctx, "repo", &s.repo)?;

    if s.events == 0 {
        line(ctx, "log", "nothing recorded yet — run `madelog init`")?;
    } else {
        let kinds: Vec<String> = s
            .events_by_kind
            .iter()
            .map(|(kind, n)| format!("{kind} {n}"))
            .collect();
        line(
            ctx,
            "log",
            &format!(
                "{} event{} chained   {}",
                s.events,
                if s.events == 1 { "" } else { "s" },
                kinds.join(", ")
            ),
        )?;
    }

    match &s.last_commit {
        Some(c) => {
            line(
                ctx,
                "last commit",
                &format!("{}  {}", short(&c.sha), c.message),
            )?;
            line(
                ctx,
                "",
                &format!(
                    "recorded {}   committed {}",
                    c.recorded_at,
                    c.committed_at.as_deref().unwrap_or("(not recorded)")
                ),
            )?;
        }
        None => line(ctx, "last commit", "none recorded yet")?,
    }

    if s.unrecorded_commits == 0 {
        line(ctx, "git", "every commit on this branch is recorded")?;
    } else {
        line(
            ctx,
            "git",
            &format!(
                "{} commit{} in git {} not recorded — run `madelog backfill`",
                s.unrecorded_commits,
                if s.unrecorded_commits == 1 { "" } else { "s" },
                if s.unrecorded_commits == 1 {
                    "is"
                } else {
                    "are"
                }
            ),
        )?;
    }

    for (i, hook) in s.hooks.iter().enumerate() {
        let label = if i == 0 { "hooks" } else { "" };
        let body = match (&hook.command, hook.installed) {
            (Some(command), true) => {
                let mark = if hook.command_exists {
                    ""
                } else {
                    "   (that file is gone — run `madelog doctor --fix`)"
                };
                format!("{:<13} calls {command}{mark}", hook.name)
            }
            _ => format!(
                "{:<13} not installed — run `madelog doctor --fix`",
                hook.name
            ),
        };
        line(ctx, label, &body)?;
    }

    for (i, tool) in s.ai_tools.iter().enumerate() {
        let label = if i == 0 { "ai tools" } else { "" };
        let body = match tool.sessions {
            Some(n) => format!(
                "{:<12} {}, {}",
                tool.name,
                tool.note,
                crate::capture::sessions_phrase(n)
            ),
            None => format!("{:<12} {}", tool.name, tool.note),
        };
        line(ctx, label, &body)?;
    }
    line(ctx, "ai sessions", &s.ai_sessions)?;

    let ollama = &s.ollama;
    let body = if ollama.provider == "none" {
        "off (llm.provider = none); summaries use the template".to_string()
    } else if !ollama.reachable {
        format!(
            "not answering at {}; summaries use the template",
            ollama.host
        )
    } else if ollama.model_ready {
        format!("{} at {}", ollama.model, ollama.host)
    } else {
        format!(
            "{} is up but {} is not pulled — run `madelog model pull`",
            ollama.host, ollama.model
        )
    };
    line(ctx, "ollama", &body)?;

    line(ctx, "server", &s.server_url)?;

    match &s.published {
        Some(p) => {
            line(ctx, "published", &p.url)?;
            let when = p.published_at.as_deref().unwrap_or("(not recorded)");
            if s.page.newer_than_publish {
                line(
                    ctx,
                    "",
                    &format!(
                        "last sent {when} — the page on disk is newer; run `madelog publish` again"
                    ),
                )?;
            } else {
                line(ctx, "", &format!("last sent {when}"))?;
            }
        }
        None => line(ctx, "published", "not published")?,
    }

    line(
        ctx,
        "page",
        &if s.page.rendered {
            s.page.path.clone()
        } else {
            format!("{} (not rendered yet)", s.page.path)
        },
    )?;
    if s.page.older_than_log {
        line(
            ctx,
            "",
            "page older than log — `madelog open` renders it again",
        )?;
    }
    Ok(())
}

fn line(ctx: &mut Ctx<'_>, label: &str, body: &str) -> Result<()> {
    ctx.say(&format!("{label:<12} {body}"))
}

fn short(sha: &str) -> String {
    sha[..7.min(sha.len())].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_url_is_the_server_plus_the_slug() {
        assert_eq!(
            page_url("https://madelog.app/", "quiet-fern-4821"),
            "https://madelog.app/p/quiet-fern-4821"
        );
    }

    #[test]
    fn a_model_matches_its_latest_tag() {
        assert!(same_model("qwen3:4b", "qwen3:4b"));
        assert!(same_model("qwen3:4b:latest", "qwen3:4b"));
        assert!(!same_model("llama3", "qwen3:4b"));
    }

    #[test]
    fn a_page_is_only_newer_when_there_is_something_to_be_newer_than() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("index.html");
        std::fs::write(&path, "<html>").unwrap();

        assert!(is_newer(&path, Some("2000-01-01T00:00:00Z")));
        assert!(!is_newer(&path, Some("2999-01-01T00:00:00Z")));
        assert!(!is_newer(&path, None), "never published, never stale");
        assert!(!is_newer(&path, Some("not a timestamp")));
        assert!(!is_newer(
            &dir.path().join("gone.html"),
            Some("2000-01-01T00:00:00Z")
        ));
    }
}
