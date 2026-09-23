//! Turning automatic update checks on and off.
//!
//! The publisher decides everything else about updating, in the manifest they
//! signed. This is the one question that belongs to whoever owns the machine,
//! and without a way to answer it the only way to stop an installation
//! checking is to delete its updater — which the next install puts back.

use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::state::UpdatePhase;
use xpack_core::{AutomaticChecks, NO_UPDATE_ENV, Result, UpdatePolicy};

use super::Context;

/// Arguments for `xpack autoupdate`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Application whose automatic checks are being changed.
    #[arg(value_name = "APPLICATION_ID")]
    pub(crate) application: String,

    /// Stop checking for updates unless asked.
    #[arg(long, conflicts_with = "on")]
    off: bool,

    /// Check for updates again.
    #[arg(long)]
    on: bool,
}

/// Runs `xpack autoupdate`.
pub(crate) fn run(args: &Args, context: &Context) -> Result<ExitCode> {
    let paths = context.paths(&args.application)?;

    if args.off || args.on {
        UpdatePolicy::automatic(args.on).save(&paths)?;
    }

    let decision = xpack_core::automatic_checks(&paths);
    crate::output::field("automatic checks", decision.describe());

    // The environment is worth naming rather than leaving someone to wonder
    // why a file they just wrote had no effect.
    if decision == AutomaticChecks::StoppedByEnvironment {
        xpack_core::errln!(
            "note: {NO_UPDATE_ENV} is set in this environment, which stops automatic checks \
             for every installation regardless of this setting"
        );
    }

    // Turning checks off does not reach back into a version already downloaded
    // and verified: it is staged, and the launcher activates it at the next
    // start. Saying so is the difference between a surprise and a decision.
    if args.off
        && let Ok(lock) = xpack_platform::InstallLock::acquire(&paths)
        && let Ok(state) = lock.load_or_new_state(&args.application)
        && let UpdatePhase::Staged { version } = &state.update
    {
        xpack_core::errln!(
            "note: {version} was already downloaded and verified, and will be used the next \
             time the application starts"
        );
    }

    super::success()
}
