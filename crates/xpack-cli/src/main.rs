//! The `xpack` command-line tool.
//!
//! # Output conventions
//!
//! Results go to standard output, diagnostics and errors to standard error, so
//! output can be piped without losing anything to interleaved logging. Every
//! listing command accepts `--json` for scripting.
//!
//! # Exit codes
//!
//! | Code | Meaning |
//! | --- | --- |
//! | 0 | success |
//! | 1 | the operation failed |
//! | 2 | the command line could not be parsed |
//! | 3 | a security check failed |
//! | 4 | another xpack operation holds the lock |
//!
//! Code 3 is separate on purpose. A script that retries on failure must not
//! retry a signature mismatch, and a monitoring system should treat it
//! differently from a full disk.

mod commands;
mod config;
mod output;
mod progress;

use clap::{Parser, Subcommand};
use xpack_core::Error;

/// Cross-platform application packaging, installation and updates.
#[derive(Parser)]
#[command(name = "xpack", version, about, long_about = None)]
struct Cli {
    /// Print more detail about what is happening.
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Installation root. Defaults to the per-user data directory.
    #[arg(long, global = true, value_name = "DIR", env = "XPACK_INSTALL_ROOT")]
    root: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate a signing key pair.
    Keygen(commands::keygen::Args),
    /// Build a signed .xpkg from a payload directory.
    Pack(commands::pack::Args),
    /// Write the update index a server publishes.
    Index(commands::index::Args),
    /// Describe a package without trusting it.
    Inspect(commands::inspect::Args),
    /// Check a package's signature against a key.
    Verify(commands::verify::Args),
    /// Install a package.
    Install(commands::install::Args),
    /// List installed versions.
    List(commands::list::Args),
    /// Launch the active version.
    Run(commands::run::Args),
    /// Make an installed version active.
    Activate(commands::activate::Args),
    /// Return to the previous healthy version.
    Rollback(commands::rollback::Args),
    /// Remove versions that are no longer needed.
    Prune(commands::prune::Args),
    /// Remove an installation.
    Uninstall(commands::uninstall::Args),
    /// Finish or undo an interrupted operation.
    Recover(commands::recover::Args),
    /// Check for and apply an update.
    Update(commands::update::Args),
}

impl Command {
    /// The installation a command acts on, when it has one.
    ///
    /// Used to decide where logs go before any work starts. Commands that
    /// operate on a package rather than an installation return `None`.
    fn application_id(&self) -> Option<String> {
        match self {
            // These build or read package files. None of them touches an
            // installation, so there is nowhere to log but the console.
            Self::Keygen(_)
            | Self::Pack(_)
            | Self::Index(_)
            | Self::Inspect(_)
            | Self::Verify(_) => None,
            Self::Install(a) => a.application_id(),
            Self::List(a) => Some(a.application.clone()),
            Self::Run(a) => Some(a.application.clone()),
            Self::Activate(a) => Some(a.application.clone()),
            Self::Rollback(a) => Some(a.application.clone()),
            Self::Prune(a) => Some(a.application.clone()),
            Self::Uninstall(a) => Some(a.application.clone()),
            Self::Recover(a) => Some(a.application.clone()),
            Self::Update(a) => Some(a.application.clone()),
        }
    }
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    let context = commands::Context { root: cli.root.clone() };

    // Logging is configured once, here, because installing a subscriber is a
    // one-time operation for the whole process: a later call is ignored, not
    // applied. So the destination has to be decided before anything is logged,
    // which means asking the command up front whether it has an installation.
    // Commands that build or inspect a package do not, and log to the console
    // alone.
    let console =
        if cli.verbose { xpack_log::Console::Verbose } else { xpack_log::Console::Normal };
    let paths = cli.command.application_id().and_then(|id| context.paths(&id).ok());
    xpack_log::init(&xpack_log::Config {
        console,
        file: paths.as_ref(),
        format: xpack_log::Format::Text,
    });

    let outcome = match &cli.command {
        Command::Keygen(a) => commands::keygen::run(a),
        Command::Pack(a) => commands::pack::run(a),
        Command::Index(a) => commands::index::run(a),
        Command::Inspect(a) => commands::inspect::run(a),
        Command::Verify(a) => commands::verify::run(a),
        Command::Install(a) => commands::install::run(a, &context),
        Command::List(a) => commands::list::run(a, &context),
        Command::Run(a) => commands::run::run(a, &context),
        Command::Activate(a) => commands::activate::run(a, &context),
        Command::Rollback(a) => commands::rollback::run(a, &context),
        Command::Prune(a) => commands::prune::run(a, &context),
        Command::Uninstall(a) => commands::uninstall::run(a, &context),
        Command::Recover(a) => commands::recover::run(a, &context),
        Command::Update(a) => commands::update::run(a, &context),
    };

    match outcome {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            // Chained causes carry the detail that makes a failure diagnosable
            // — which file, which syscall — so they are printed rather than
            // collapsed into the top-level message.
            let mut source = std::error::Error::source(&error);
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            exit_code_for(&error)
        }
    }
}

/// Maps a failure to the exit code a script should see.
fn exit_code_for(error: &Error) -> std::process::ExitCode {
    if error.is_integrity_failure() {
        return std::process::ExitCode::from(3);
    }
    match error {
        Error::Locked(_) => std::process::ExitCode::from(4),
        _ => std::process::ExitCode::FAILURE,
    }
}
