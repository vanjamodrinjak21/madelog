//! `madelog open` — the page, in the browser you already use.
//!
//! No server, no localhost, no port: the page is one self-contained file, so
//! the browser opens it off the disk. If it has not been rendered yet, it is
//! rendered first — asking someone to run `madelog publish --local` before
//! `madelog open` would be a riddle, not a workflow.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::gitrepo::Repo;
use crate::Ctx;

/// Override the command that opens the page. Set by tests; useful to anyone
/// whose desktop disagrees with the default.
pub const OPENER_ENV: &str = "MADELOG_OPENER";

pub fn run(ctx: &mut Ctx<'_>, print_only: bool) -> Result<()> {
    let root = Repo::discover(&ctx.cwd)?.workdir()?;
    if !root.join(".madelog").join("log.db").exists() {
        bail!(
            "there is no madelog log in {} — run `madelog init` first, then commit something.",
            root.display()
        );
    }

    let path = root.join(".madelog").join("site").join("index.html");
    let path = if path.exists() {
        path
    } else {
        // Nothing rendered yet, so render it. Locally only: `madelog open`
        // never puts anything online.
        crate::page::render_local(&root, None)?
    };

    if print_only {
        ctx.say(&path.display().to_string())?;
        return Ok(());
    }

    let (program, args) = opener(&path);
    Command::new(&program)
        .args(&args)
        .status()
        .with_context(|| format!("running `{program}` to open {}", path.display()))
        .and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                bail!(
                    "`{program}` could not open {}. Open it by hand: {}",
                    path.display(),
                    path.display()
                )
            }
        })?;

    ctx.say(&format!("opened {}", path.display()))?;
    Ok(())
}

/// The command that opens a file in the desktop's default application.
fn opener(path: &Path) -> (String, Vec<String>) {
    if let Some(custom) = std::env::var_os(OPENER_ENV) {
        let custom = custom.to_string_lossy().into_owned();
        if !custom.trim().is_empty() {
            let mut parts = custom.split_whitespace().map(str::to_string);
            let program = parts.next().unwrap_or_else(|| custom.clone());
            let mut args: Vec<String> = parts.collect();
            args.push(path.display().to_string());
            return (program, args);
        }
    }
    default_opener(path)
}

#[cfg(target_os = "macos")]
fn default_opener(path: &Path) -> (String, Vec<String>) {
    ("open".to_string(), vec![path.display().to_string()])
}

#[cfg(target_os = "windows")]
fn default_opener(path: &Path) -> (String, Vec<String>) {
    (
        "cmd".to_string(),
        vec![
            "/C".to_string(),
            "start".to_string(),
            // `start` reads the first quoted argument as a window title.
            String::new(),
            path.display().to_string(),
        ],
    )
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn default_opener(path: &Path) -> (String, Vec<String>) {
    ("xdg-open".to_string(), vec![path.display().to_string()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_opener_is_the_desktop_one() {
        let (program, args) = default_opener(Path::new("/tmp/index.html"));
        if cfg!(target_os = "macos") {
            assert_eq!(program, "open");
            assert_eq!(args, vec!["/tmp/index.html"]);
        } else if cfg!(target_os = "windows") {
            assert_eq!(program, "cmd");
            assert_eq!(args[0], "/C");
            assert_eq!(args[1], "start");
            assert_eq!(args.last().unwrap(), "/tmp/index.html");
        } else {
            assert_eq!(program, "xdg-open");
            assert_eq!(args, vec!["/tmp/index.html"]);
        }
    }

    #[test]
    fn the_environment_can_name_another_opener() {
        std::env::set_var(OPENER_ENV, "/bin/echo opened");
        let (program, args) = opener(Path::new("/tmp/index.html"));
        std::env::remove_var(OPENER_ENV);
        assert_eq!(program, "/bin/echo");
        assert_eq!(args, vec!["opened", "/tmp/index.html"]);
    }
}
