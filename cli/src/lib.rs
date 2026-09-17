//! `pop` — record how your project came together, and prove it.
//!
//! The binary is a thin shell over this library so that integration tests can
//! drive commands against a temporary repository without spawning a process.

pub mod chain;
pub mod cli;
pub mod commands;
pub mod config;
pub mod event;
pub mod gitai;
pub mod gitrepo;
pub mod keys;
pub mod store;

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// An error that carries the process exit code pop should use.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ExitError {
    pub code: i32,
    pub message: String,
}

impl ExitError {
    pub fn new(code: i32, message: impl Into<String>) -> ExitError {
        ExitError {
            code,
            message: message.into(),
        }
    }

    /// The command exists but does nothing in this version.
    pub fn unavailable(command: &str) -> anyhow::Error {
        ExitError::new(2, format!("`{command}` is not available in this version")).into()
    }

    /// The command is a placeholder a later task fills in.
    pub fn not_implemented(command: &str) -> anyhow::Error {
        ExitError::new(2, format!("`{command}` is not implemented yet")).into()
    }
}

/// Everything a command is allowed to touch: a directory, an input stream and
/// an output stream. No globals, so tests stay honest.
pub struct Ctx<'a> {
    /// The directory pop was invoked from.
    pub cwd: PathBuf,
    pub input: &'a mut dyn BufRead,
    pub out: &'a mut dyn Write,
}

impl Ctx<'_> {
    pub fn say(&mut self, line: &str) -> Result<()> {
        writeln!(self.out, "{line}").context("writing output")
    }

    pub fn blank(&mut self) -> Result<()> {
        writeln!(self.out).context("writing output")
    }

    /// Ask a question and read one line. Returns `default` on an empty answer
    /// or on end of input.
    pub fn ask(&mut self, question: &str, default: &str) -> Result<String> {
        write!(self.out, "{question}")?;
        self.out.flush()?;
        let mut line = String::new();
        let read = self.input.read_line(&mut line)?;
        if read == 0 {
            return Ok(default.to_string());
        }
        let answer = line.trim();
        Ok(if answer.is_empty() {
            default.to_string()
        } else {
            answer.to_string()
        })
    }

    /// Ask a yes/no question. `default_yes` decides what an empty answer means.
    pub fn ask_yes_no(&mut self, question: &str, default_yes: bool) -> Result<bool> {
        let suffix = if default_yes { "[Y/n]" } else { "[y/N]" };
        let answer = self.ask(&format!("{question} {suffix} "), "")?;
        Ok(match answer.trim().to_ascii_lowercase().as_str() {
            "" => default_yes,
            "y" | "yes" => true,
            _ => false,
        })
    }

    /// The `.pop` directory of the repository containing [`Ctx::cwd`].
    pub fn pop_dir(&self) -> Result<PathBuf> {
        Ok(gitrepo::Repo::discover(&self.cwd)?.workdir()?.join(".pop"))
    }

    /// Open the log, insisting that `pop init` has already run.
    pub fn open_store(&self) -> Result<store::Store> {
        let db = self.pop_dir()?.join("log.db");
        if !db.exists() {
            anyhow::bail!(
                "no pop log in this repository yet — run `pop init` first ({} is missing)",
                db.display()
            );
        }
        store::Store::open(&db)
    }
}

/// Run a command with pop's own stdin and stdout.
pub fn run_in(root: &Path, args: &[&str]) -> Result<()> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    run_ctx(root, args, &mut input, &mut out)
}

/// Run a command against explicit input and output streams.
pub fn run_in_with<R: Read, W: Write>(
    root: &Path,
    args: &[&str],
    input: R,
    out: &mut W,
) -> Result<()> {
    let mut input = BufReader::new(input);
    run_ctx(root, args, &mut input, out)
}

/// Run a command and return whatever it printed.
pub fn run_capture(root: &Path, args: &[&str]) -> Result<String> {
    let mut out: Vec<u8> = Vec::new();
    run_in_with(root, args, std::io::empty(), &mut out)?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn run_ctx(
    root: &Path,
    args: &[&str],
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<()> {
    let parsed = cli::parse(args)?;
    let mut ctx = Ctx {
        cwd: root.to_path_buf(),
        input,
        out,
    };
    cli::dispatch(parsed, &mut ctx)
}

/// The path of the running `pop` binary, honouring `$POP_EXE`.
///
/// The per-project installer drops the binary at `<repo>/.pop/bin/pop`; the
/// post-commit hook then has to call that path rather than whatever `pop`
/// happens to be on PATH.
pub fn current_exe() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("POP_EXE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    std::env::current_exe().ok()
}
