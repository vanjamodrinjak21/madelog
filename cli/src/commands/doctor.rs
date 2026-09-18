//! `madelog doctor` — is anything broken, and can madelog fix it?
//!
//! Every check prints one line that starts with its verdict, so the output
//! greps. A `fail` means madelog's own setup is broken and the log or the page
//! will suffer; a `warn` means an optional tool is missing and madelog carries
//! on with less. Only a `fail` sets the exit code, because a machine without
//! Ollama is not a sick machine.
//!
//! `--fix` repairs the three things that are safe to repair without asking:
//! the git hooks, the `.git/info/exclude` entry, and the mode of the device
//! key. Nothing here ever touches the log.

use std::path::Path;

use anyhow::Result;

use crate::chain::Verification;
use crate::commands::init;
use crate::config::Config;
use crate::gitrepo::Repo;
use crate::keys::DeviceKey;
use crate::store::Store;
use crate::{Ctx, ExitError};

/// How a check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing to do.
    Ok,
    /// Something optional is missing; madelog works with less.
    Warn,
    /// madelog's own setup is broken. This is what sets the exit code.
    Fail,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Ok => "ok  ",
            Verdict::Warn => "warn",
            Verdict::Fail => "FAIL",
        }
    }
}

struct Report {
    lines: Vec<(Verdict, String, String)>,
}

impl Report {
    fn new() -> Report {
        Report { lines: Vec::new() }
    }

    fn add(&mut self, verdict: Verdict, name: &str, detail: impl Into<String>) {
        self.lines.push((verdict, name.to_string(), detail.into()));
    }

    fn failed(&self) -> usize {
        self.lines
            .iter()
            .filter(|(v, _, _)| *v == Verdict::Fail)
            .count()
    }

    fn warned(&self) -> usize {
        self.lines
            .iter()
            .filter(|(v, _, _)| *v == Verdict::Warn)
            .count()
    }
}

pub fn run(ctx: &mut Ctx<'_>, fix: bool) -> Result<()> {
    let repo = Repo::discover(&ctx.cwd)?;
    let root = repo.workdir()?;
    let git_dir = repo.git_dir();
    let madelog_dir = root.join(".madelog");
    let config = Config::load(&madelog_dir.join("config.toml"))?;

    let mut report = Report::new();
    check_log(&mut report, &madelog_dir);
    check_hooks(&mut report, &root, &git_dir, fix);
    check_exclude(&mut report, &git_dir, fix);
    check_device_key(&mut report, fix);
    check_ai_tools(&mut report, &root);
    check_ollama(&mut report, &config);
    check_server(&mut report, &config);

    for (verdict, name, detail) in &report.lines {
        ctx.say(&format!("{}  {name:<14} {detail}", verdict.label()))?;
    }

    ctx.blank()?;
    let failed = report.failed();
    let warned = report.warned();
    if failed == 0 && warned == 0 {
        ctx.say("everything checks out")?;
        return Ok(());
    }
    if failed == 0 {
        ctx.say(&format!(
            "{warned} thing{} madelog can live without {} missing; nothing is broken",
            if warned == 1 { "" } else { "s" },
            if warned == 1 { "is" } else { "are" }
        ))?;
        return Ok(());
    }
    // The summary line *is* the error message: printing it twice, once as
    // output and once as the exit error, would only look like two problems.
    Err(ExitError::new(
        1,
        format!(
            "madelog: {failed} check{} failed{}",
            if failed == 1 { "" } else { "s" },
            if fix {
                ""
            } else {
                " — `madelog doctor --fix` repairs the hooks, the git exclude entry and the key mode"
            }
        ),
    )
    .into())
}

// ---------------------------------------------------------------------------
// the checks
// ---------------------------------------------------------------------------

fn check_log(report: &mut Report, madelog_dir: &Path) {
    let db = madelog_dir.join("log.db");
    if !db.exists() {
        report.add(
            Verdict::Fail,
            "log",
            format!(
                "{} is missing — run `madelog init` in this repository",
                db.display()
            ),
        );
        return;
    }
    let store = match Store::open(&db) {
        Ok(store) => store,
        Err(err) => {
            report.add(
                Verdict::Fail,
                "log",
                format!("{} will not open: {err:#}", db.display()),
            );
            return;
        }
    };
    match store.verify() {
        Ok(Verification::Ok { count, root }) => report.add(
            Verdict::Ok,
            "log",
            format!(
                "{count} events, chain verifies, root {}",
                &root[..12.min(root.len())]
            ),
        ),
        Ok(Verification::Broken { seq, reason, .. }) => report.add(
            Verdict::Fail,
            "log",
            format!("the chain is broken at seq {seq}: {reason}"),
        ),
        Err(err) => report.add(Verdict::Fail, "log", format!("cannot be read: {err:#}")),
    }
}

fn check_hooks(report: &mut Report, root: &Path, git_dir: &Path, fix: bool) {
    let wanted = init::hook_command_for(root, crate::current_exe().as_deref());
    for (name, _) in init::HOOKS {
        let installed = init::hook_is_installed(git_dir, name);
        let command = init::installed_hook_command(git_dir, name);
        let runnable = command.as_deref().is_some_and(exists);

        if installed && runnable {
            report.add(
                Verdict::Ok,
                name,
                format!("calls {}", command.unwrap_or_default()),
            );
            continue;
        }

        if !fix {
            let why = if installed {
                format!(
                    "calls {}, which is not there any more",
                    command.unwrap_or_default()
                )
            } else {
                "not installed".to_string()
            };
            report.add(
                Verdict::Fail,
                name,
                format!("{why} — `madelog doctor --fix` reinstalls it"),
            );
            continue;
        }

        // Reinstalling means replacing our own lines, not the user's: strip
        // what we wrote before, then write it again with today's binary.
        match reinstall(git_dir, name, &wanted) {
            Ok(()) => report.add(Verdict::Ok, name, format!("reinstalled, calls {wanted}")),
            Err(err) => report.add(Verdict::Fail, name, format!("could not be fixed: {err:#}")),
        }
    }
}

/// Remove madelog's two lines from `.git/hooks/<name>` and put them back.
fn reinstall(git_dir: &Path, name: &str, command: &str) -> Result<()> {
    let path = git_dir.join("hooks").join(name);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let marker = init::hook_marker(name);
        let mut kept: Vec<&str> = Vec::new();
        let mut skip_next = false;
        for line in existing.lines() {
            if skip_next {
                skip_next = false;
                continue;
            }
            if line.trim_start().starts_with(&marker) {
                // Our comment, and the call that follows it.
                skip_next = true;
                continue;
            }
            kept.push(line);
        }
        let mut body = kept.join("\n");
        if !body.ends_with('\n') {
            body.push('\n');
        }
        std::fs::write(&path, body)?;
    }
    init::install_hook(git_dir, name, command)
}

fn check_exclude(report: &mut Report, git_dir: &Path, fix: bool) {
    let path = git_dir.join("info").join("exclude");
    let excluded = std::fs::read_to_string(&path)
        .map(|body| body.lines().any(|l| l.trim() == ".madelog/"))
        .unwrap_or(false);
    if excluded {
        report.add(Verdict::Ok, "git exclude", ".madelog/ is excluded");
        return;
    }
    if !fix {
        report.add(
            Verdict::Fail,
            "git exclude",
            ".madelog/ is not in .git/info/exclude, so git will offer to commit your log — \
             `madelog doctor --fix` adds it",
        );
        return;
    }
    match init::add_to_git_exclude(git_dir) {
        Ok(()) => report.add(Verdict::Ok, "git exclude", ".madelog/ added"),
        Err(err) => report.add(
            Verdict::Fail,
            "git exclude",
            format!("could not be fixed: {err:#}"),
        ),
    }
}

fn check_device_key(report: &mut Report, fix: bool) {
    let path = match crate::keys::key_path() {
        Ok(path) => path,
        Err(err) => {
            report.add(Verdict::Fail, "device key", format!("{err:#}"));
            return;
        }
    };
    if !path.exists() {
        if !fix {
            report.add(
                Verdict::Fail,
                "device key",
                format!(
                    "{} is missing, so `madelog publish` cannot sign — `madelog doctor --fix` \
                     creates one",
                    path.display()
                ),
            );
            return;
        }
        if let Err(err) = DeviceKey::load_or_create() {
            report.add(Verdict::Fail, "device key", format!("{err:#}"));
            return;
        }
    }

    match key_mode(&path) {
        None => report.add(
            Verdict::Ok,
            "device key",
            format!("{} is there", path.display()),
        ),
        Some(0o600) => report.add(
            Verdict::Ok,
            "device key",
            format!("{} is there, mode 0600", path.display()),
        ),
        Some(mode) if fix => match restrict(&path) {
            Ok(()) => report.add(
                Verdict::Ok,
                "device key",
                format!("{} was {mode:04o}, now 0600", path.display()),
            ),
            Err(err) => report.add(
                Verdict::Fail,
                "device key",
                format!(
                    "{} is {mode:04o} and could not be fixed: {err:#}",
                    path.display()
                ),
            ),
        },
        Some(mode) => report.add(
            Verdict::Fail,
            "device key",
            format!(
                "{} is mode {mode:04o}; it is your signing key and should be 0600 — \
                 `madelog doctor --fix` sets it",
                path.display()
            ),
        ),
    }
}

#[cfg(unix)]
fn key_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(std::fs::metadata(path).ok()?.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn key_mode(_path: &Path) -> Option<u32> {
    // Windows has no mode to check, and inventing one would only lie.
    None
}

#[cfg(unix)]
fn restrict(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict(_path: &Path) -> Result<()> {
    Ok(())
}

/// Which AI tools left a log madelog can read, and whether their sessions are
/// being recorded here.
///
/// None of this is a `fail`: a machine with no AI tool on it is a perfectly
/// healthy machine, and a page without AI sessions is a page that says so.
fn check_ai_tools(report: &mut Report, root: &Path) {
    let tools = crate::capture::report(Some(root));
    for tool in &tools {
        let detail = match tool.sessions {
            Some(n) => format!("{}, {}", tool.note, crate::capture::sessions_phrase(n)),
            None => tool.note.clone(),
        };
        let verdict = if tool.installed {
            Verdict::Ok
        } else {
            Verdict::Warn
        };
        report.add(verdict, &tool.name, detail);
    }

    let (recording, why) = crate::commands::hook::ai_session_state(root);
    report.add(
        if recording {
            Verdict::Ok
        } else {
            Verdict::Warn
        },
        "ai sessions",
        why,
    );
}

fn check_ollama(report: &mut Report, config: &Config) {
    if config.llm.provider == "none" {
        report.add(
            Verdict::Ok,
            "ollama",
            "off by configuration (llm.provider = none)",
        );
        return;
    }
    let host = crate::summarize::ollama::host(config);
    let client = crate::summarize::Ollama::new(&host);
    match client.models() {
        Ok(models) if models.iter().any(|m| same_model(m, &config.llm.model)) => {
            report.add(
                Verdict::Ok,
                "ollama",
                format!("{host}, {} is pulled", config.llm.model),
            );
        }
        Ok(_) => report.add(
            Verdict::Warn,
            "ollama",
            format!(
                "{host} answers but {} is not pulled, so summaries use the template",
                config.llm.model
            ),
        ),
        Err(_) => report.add(
            Verdict::Warn,
            "ollama",
            format!("nothing answering at {host}, so summaries use the template"),
        ),
    }
}

fn same_model(have: &str, want: &str) -> bool {
    let strip = |s: &str| s.trim().trim_end_matches(":latest").to_string();
    strip(have) == strip(want)
}

fn check_server(report: &mut Report, config: &Config) {
    let url = config.server.url.trim();
    if url.is_empty() {
        report.add(
            Verdict::Warn,
            "server",
            "no server.url, so `madelog publish` can only write locally",
        );
        return;
    }
    let client = crate::publish::Client::new(url);
    match client.healthz() {
        Ok(200) => report.add(Verdict::Ok, "server", format!("{} answers", client.base())),
        Ok(status) => report.add(
            Verdict::Warn,
            "server",
            format!("{}/healthz answered {status}", client.base()),
        ),
        Err(_) => report.add(
            Verdict::Warn,
            "server",
            format!(
                "{}/healthz did not answer within {}s; publishing will fail until it does",
                client.base(),
                crate::publish::HEALTH_TIMEOUT.as_secs()
            ),
        ),
    }
}

fn exists(command: &str) -> bool {
    if command.contains('/') || command.contains('\\') {
        return Path::new(command).is_file();
    }
    init::which(command).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reinstalling_replaces_our_lines_and_keeps_the_rest() {
        let dir = tempfile::TempDir::new().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(git_dir.join("hooks")).unwrap();
        std::fs::write(
            git_dir.join("hooks/post-commit"),
            "#!/bin/sh\necho mine\n# madelog-cli:post-commit - old\n/gone/madelog hook post-commit || true\n",
        )
        .unwrap();

        reinstall(&git_dir, "post-commit", "/new/madelog").unwrap();

        let body = std::fs::read_to_string(git_dir.join("hooks/post-commit")).unwrap();
        assert!(body.contains("echo mine"), "{body}");
        assert!(!body.contains("/gone/madelog"), "{body}");
        assert!(
            body.contains("/new/madelog hook post-commit || true"),
            "{body}"
        );
        assert_eq!(
            body.matches(&init::hook_marker("post-commit")).count(),
            1,
            "{body}"
        );
    }

    #[test]
    fn a_command_with_a_path_has_to_be_a_file() {
        assert!(!exists("/definitely/not/here/madelog"));
        assert!(exists("sh") || exists("cmd"));
    }
}
