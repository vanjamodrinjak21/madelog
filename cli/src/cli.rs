//! Argument parsing and dispatch. Every user-facing string here is English.

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::{commands, Ctx, ExitError};

#[derive(Debug, Parser)]
#[command(
    name = "madelog",
    version,
    about = "Record how your project came together, and prove it.",
    long_about = "madelog runs alongside git. It records your commits, your AI sessions and the \
one-sentence decisions you write down, chains them with sha256, and turns them into a page you \
can send to a company. Nothing leaves your machine until you run `madelog publish`."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Set up madelog in this repository.
    Init(InitArgs),
    /// Explain what madelog does, check your tools, and offer to set up this repo.
    Onboard(OnboardArgs),
    /// Write down a one-sentence decision.
    Note(NoteArgs),
    /// Hooks madelog installs into git. You do not run these by hand.
    #[command(subcommand)]
    Hook(HookCommand),
    /// Print the recorded events.
    Log(LogArgs),
    /// Recompute the hash chain and print the root.
    Verify,
    /// Write everything madelog has recorded as JSON.
    Export(ExportArgs),
    /// Hide one AI session from the published page.
    Hide(HideArgs),
    /// Read or change a configuration value.
    Config(ConfigArgs),
    /// Render the project page and publish it.
    Publish(PublishArgs),
    /// Take the published page down again.
    Unpublish(UnpublishArgs),
    /// Look for patterns in the log and record them.
    Analyze(AnalyzeArgs),
    /// Write weekly summaries.
    Summarize(SummarizeArgs),
    /// Work with existing summaries.
    #[command(subcommand)]
    Summary(SummaryCommand),
    /// Manage the local language model.
    #[command(subcommand)]
    Model(ModelCommand),
    /// Ask a question about this project's history.
    Ask(AskArgs),
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// What are you building?
    #[arg(long)]
    pub what: Option<String>,
    /// Who is it for?
    #[arg(long = "for-whom")]
    pub for_whom: Option<String>,
    /// Which AI tools do you use, comma separated.
    #[arg(long)]
    pub tools: Option<String>,
    /// Record AI sessions from Git AI.
    #[arg(long = "record-ai-sessions")]
    pub record_ai_sessions: bool,
    /// Do not record AI sessions.
    #[arg(long = "no-record-ai-sessions", conflicts_with = "record_ai_sessions")]
    pub no_record_ai_sessions: bool,
    /// Do not ask anything; take the answers given and defaults for the rest.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct OnboardArgs {
    /// Per-project install: set this repository up without asking.
    #[arg(long)]
    pub project: bool,
    /// Do not ask anything.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct NoteArgs {
    /// The decision, in one sentence.
    #[arg(required = true, num_args = 1.., trailing_var_arg = true)]
    pub text: Vec<String>,
    /// Keep this note out of the published page.
    #[arg(long)]
    pub private: bool,
}

#[derive(Debug, Subcommand)]
pub enum HookCommand {
    /// Record the commit that was just made.
    PostCommit,
}

#[derive(Debug, Args)]
pub struct LogArgs {
    /// Print the events as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Where to write the JSON. Prints to stdout when omitted.
    pub path: Option<String>,
}

#[derive(Debug, Args)]
pub struct HideArgs {
    /// The id of the ai_session event to hide.
    pub event_id: String,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print a configuration value.
    Get { key: String },
    /// Change a configuration value.
    Set { key: String, value: String },
    /// Print every key and its value.
    List,
}

#[derive(Debug, Args)]
pub struct PublishArgs {
    /// Render the page to .madelog/site/index.html and stop there.
    #[arg(long)]
    pub local: bool,
    /// The server to publish to.
    #[arg(long)]
    pub server: Option<String>,
    /// Skip the preview.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct UnpublishArgs {
    /// The server the page is on.
    #[arg(long)]
    pub server: Option<String>,
}

#[derive(Debug, Args)]
pub struct AnalyzeArgs {
    /// Print the patterns and moments as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct SummarizeArgs {
    /// Only rebuild one week, counted from the first commit.
    #[arg(long)]
    pub week: Option<u32>,
    /// Write a new summary even for weeks that already have one.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Subcommand)]
pub enum SummaryCommand {
    /// Edit a summary in $EDITOR.
    Edit { event_id: String },
}

#[derive(Debug, Subcommand)]
pub enum ModelCommand {
    /// Download the local model.
    Pull,
}

#[derive(Debug, Args)]
pub struct AskArgs {
    #[arg(num_args = 0.., trailing_var_arg = true)]
    pub question: Vec<String>,
}

/// Parse arguments, without the leading program name.
pub fn parse(args: &[&str]) -> Result<Cli> {
    let argv = std::iter::once("madelog").chain(args.iter().copied());
    Cli::try_parse_from(argv).map_err(|e| {
        // `--help` and `--version` are not errors: print and exit 0.
        let code = match e.kind() {
            clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => 0,
            _ => 2,
        };
        ExitError::new(code, e.render().to_string()).into()
    })
}

pub fn dispatch(cli: Cli, ctx: &mut Ctx<'_>) -> Result<()> {
    match cli.command {
        Command::Init(args) => commands::init::run(ctx, commands::init::Options::from_args(&args)),
        Command::Onboard(args) => commands::onboard::run(ctx, args.project, args.yes),
        Command::Note(args) => commands::note::run(ctx, &args.text.join(" "), args.private),
        Command::Hook(HookCommand::PostCommit) => commands::hook::post_commit(ctx),
        Command::Log(args) => commands::log::run(ctx, args.json),
        Command::Verify => commands::verify::run(ctx),
        Command::Export(args) => commands::export::run(ctx, args.path.as_deref()),
        Command::Hide(args) => commands::hide::run(ctx, &args.event_id),
        Command::Config(args) => commands::config::run(ctx, args.command),
        Command::Publish(args) => commands::publish::run(
            ctx,
            commands::publish::Options {
                local: args.local,
                server: args.server,
                yes: args.yes,
            },
        ),
        Command::Unpublish(args) => commands::publish::unpublish(
            ctx,
            commands::publish::UnpublishOptions {
                server: args.server,
            },
        ),
        Command::Analyze(args) => commands::analyze::run(ctx, args.json),
        Command::Summarize(args) => commands::summarize::run(ctx, args.week, args.force),
        Command::Summary(SummaryCommand::Edit { event_id }) => {
            commands::summarize::edit(ctx, &event_id)
        }
        Command::Model(ModelCommand::Pull) => commands::model::pull(ctx),
        Command::Ask(args) => commands::ask::run(ctx, &args.question.join(" ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_tree_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn note_takes_a_sentence_and_a_flag() {
        let cli = parse(&["note", "--private", "chose sqlite"]).unwrap();
        match cli.command {
            Command::Note(a) => {
                assert_eq!(a.text.join(" "), "chose sqlite");
                assert!(a.private);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_two_ai_session_flags_conflict() {
        assert!(parse(&["init", "--record-ai-sessions", "--no-record-ai-sessions"]).is_err());
    }

    #[test]
    fn an_unknown_command_exits_with_two() {
        let err = parse(&["frobnicate"]).unwrap_err();
        assert_eq!(err.downcast_ref::<ExitError>().unwrap().code, 2);
    }
}
