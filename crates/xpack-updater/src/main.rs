//! The updater binary.
//!
//! Started detached by the launcher, with no terminal and nobody watching.
//! Every diagnostic goes to the installation's log; the exit code is the only
//! thing a caller can read, and it never causes the application any trouble.

use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use xpack_core::InstallPaths;
use xpack_update::{HttpsTransport, Timeouts};
use xpack_updater::{BackgroundUpdater, Outcome};

/// Exit codes. Anything a caller might branch on gets its own.
///
/// These match `xpack` and `xpack-installer` deliberately. A monitoring system
/// watching a fleet sees all three binaries, and a code that meant "busy" in
/// one and "you typed the command wrong" in another would be read wrong
/// exactly when it mattered. `2` is reserved for a command line clap could not
/// parse, which is why busy is 4 rather than the next free number.
mod exit {
    /// A version was staged.
    pub(crate) const STAGED: u8 = 0;
    /// Nothing to do: not due, already current, or no server configured.
    pub(crate) const NOTHING_TO_DO: u8 = 0;
    /// The update could not be completed.
    pub(crate) const FAILED: u8 = 1;
    /// A signature or digest did not verify.
    ///
    /// Separate from a plain failure because a caller that retries on failure
    /// must never retry this one: a package that fails verification will fail
    /// it again, and repeating the download only feeds an attacker or a broken
    /// mirror.
    pub(crate) const INTEGRITY: u8 = 3;
    /// Another xPack operation is using the installation.
    pub(crate) const BUSY: u8 = 4;
}

#[derive(Parser)]
#[command(name = "xpack-updater", about = "Checks for and stages xPack updates in the background")]
struct Args {
    /// Installation to update. Defaults to the one containing this binary.
    #[arg(long, value_name = "DIR")]
    application_dir: Option<std::path::PathBuf>,

    /// Minimum hours between checks, overriding what the manifest declares.
    #[arg(long, value_name = "HOURS")]
    interval: Option<u64>,

    /// Check now, whatever the interval says.
    #[arg(long)]
    force: bool,

    /// Give up after this many minutes.
    #[arg(long, value_name = "MINUTES", default_value_t = 30)]
    timeout: u64,

    /// Write one JSON object per line to standard output as work happens.
    ///
    /// This is how a desktop application shows an update: spawn this binary,
    /// read the stream a line at a time, and drive your own progress dialog in
    /// your own toolkit. xPack draws no windows of its own — a second,
    /// foreign-looking one that could not be themed or localised to match
    /// would be worse than none, and it would put a windowing stack inside a
    /// binary that must stay small and cross-compile cleanly.
    ///
    /// Without it the updater stays silent, which is what a background run
    /// spawned by the launcher wants.
    #[arg(long)]
    progress: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();

    let paths = match &args.application_dir {
        Some(dir) => InstallPaths::from_application_dir(dir),
        None => match InstallPaths::discover() {
            Ok(paths) => paths,
            Err(error) => {
                // No installation means nowhere to log, so this one line is
                // the only place it can go.
                eprintln!("xpack-updater: {error}");
                return ExitCode::from(exit::FAILED);
            }
        },
    };

    // Silent console on purpose. This process has no terminal of its own, and
    // anything it printed would land in whatever the launcher was attached to
    // — which is the user's application.
    xpack_log::init(&xpack_log::Config {
        console: xpack_log::Console::Silent,
        file: Some(&paths),
        format: xpack_log::Format::Text,
    });

    let timeouts = match Timeouts::with_total(Duration::from_secs(args.timeout * 60)) {
        Ok(timeouts) => timeouts,
        Err(error) => {
            tracing::error!(%error, "invalid timeout");
            return ExitCode::from(exit::FAILED);
        }
    };
    let transport = HttpsTransport::with_timeouts(timeouts);

    let stream = xpack_core::JsonProgress::to_stdout();
    let reporter: &dyn xpack_core::ProgressReporter =
        if args.progress { &stream } else { &xpack_core::NoProgress };

    let mut updater =
        BackgroundUpdater::new(&paths, &transport).forced(args.force).reporting_to(reporter);
    if let Some(hours) = args.interval {
        // Saturating, because a number large enough to overflow means the same
        // thing as the largest one that fits: never check again.
        updater = updater.every(Duration::from_secs(hours.saturating_mul(60 * 60)));
    }

    match updater.run() {
        Ok(Outcome::Staged(version)) => {
            tracing::info!(%version, "staged; it will be used the next time the application starts");
            ExitCode::from(exit::STAGED)
        }
        Ok(outcome) => {
            tracing::debug!(?outcome, "nothing to do");
            ExitCode::from(exit::NOTHING_TO_DO)
        }
        // A busy installation is the normal case, not a fault: the user is
        // installing, rolling back or updating by hand right now. Trying again
        // at the next application start is the whole answer.
        Err(xpack_core::Error::Locked(_)) => {
            tracing::debug!("the installation is busy; will try again next time");
            ExitCode::from(exit::BUSY)
        }
        Err(error) if error.is_integrity_failure() => {
            tracing::error!(%error, "update failed verification");
            ExitCode::from(exit::INTEGRITY)
        }
        Err(error) => {
            tracing::error!(%error, "update failed");
            ExitCode::from(exit::FAILED)
        }
    }
}
