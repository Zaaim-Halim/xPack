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
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    let context = commands::Context { root: cli.root.clone() };

    let outcome = match &cli.command {
        Command::Keygen(a) => commands::keygen::run(a),
        Command::Pack(a) => commands::pack::run(a),
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

/// Sends logs to standard error so standard output stays pipeable.
fn init_logging(verbose: bool) {
    use tracing_subscriber::EnvFilter;

    let default = if verbose { "xpack=debug" } else { "xpack=info" };
    let filter = EnvFilter::try_from_env("XPACK_LOG").unwrap_or_else(|_| EnvFilter::new(default));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .without_time()
        .with_target(false)
        .try_init();
}
