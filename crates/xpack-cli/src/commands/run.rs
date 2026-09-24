//! Launching the active version.
//!
//! # The health-check policy lives here, not in the engine
//!
//! Activating a version puts it on probation, and something must decide
//! whether probation passed. `xpack-install` deliberately takes no view: it
//! exposes the primitives and lets each caller choose.
//!
//! This command chooses the simplest defensible policy: **run the application
//! to completion and treat a zero exit status as healthy**, unless the
//! application said it started, which is healthy whatever it exits with. The
//! same report the launcher accepts, read the same way: a command-line tool's
//! exit status is its answer, and one that reported its start and then
//! answered "no" is not a failed update.
//!
//! It is *not* right for a long-running desktop application, where the version
//! would only be committed when the user eventually quits — and a crash three
//! days later would trigger a rollback long after the update stopped being the
//! explanation. Solving that properly needs the application to report its own
//! health, which is the update channel's job rather than this command's.
//!
//! So the policy is stated rather than hidden, and belongs to this command
//! alone. A launcher that watches a detached process is free to choose
//! differently without changing anything below it.

use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::{Error, Manifest, Result};
use xpack_install::Installer;
use xpack_platform::{LaunchRequest, launch};

use super::Context;

/// Arguments for `xpack run`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Application to launch.
    #[arg(value_name = "APPLICATION_ID")]
    pub(crate) application: String,

    /// Arguments passed through to the application.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    arguments: Vec<String>,

    /// Start the application without resolving probation either way.
    #[arg(long)]
    no_health_check: bool,
}

/// Runs `xpack run`.
pub(crate) fn run(args: &Args, context: &Context) -> Result<ExitCode> {
    let lock = context.lock(&args.application)?;
    let installer = Installer::new(&lock);
    installer.recover()?;

    let state = lock.load_or_new_state(&args.application)?;
    let version = state.active()?.clone();
    let paths = lock.paths();

    if !installer.is_usable(&version) {
        return Err(Error::invalid(
            "installation",
            format!("{version} is active but its files are missing; reinstall it to repair"),
        ));
    }

    // The manifest recorded at install time describes how to launch, so the
    // package is not needed again and the launch matches what was verified.
    let manifest_bytes = std::fs::read(paths.version_manifest_file(&version))
        .map_err(|e| Error::io(paths.version_manifest_file(&version), e))?;
    let manifest = Manifest::from_slice(&manifest_bytes)?;

    let on_probation = !state.update.is_idle() && !args.no_health_check;

    // The attempt is recorded *before* the launch, so a crash that prevents
    // the process from ever returning is still counted. A counter incremented
    // afterwards would never see it.
    if on_probation {
        let phase = installer.begin_attempt()?;
        if phase.attempts_exhausted() {
            xpack_core::errln!("this version has already failed to start; rolling back");
            if let Some(restored) = installer.record_failure("exhausted its startup attempts")? {
                xpack_core::errln!("rolled back to {restored}; run again to start it");
            } else {
                xpack_core::errln!("error: no healthy version to roll back to");
            }
            return Ok(ExitCode::FAILURE);
        }
    }

    // Removed first, so a report left by an earlier run is not read as this
    // one's.
    let report = paths.health_file(&version);
    let _ = std::fs::remove_file(&report);
    let request = LaunchRequest::new(paths.version_dir(&version), manifest.launch.clone())
        .with_user_arguments(args.arguments.clone())
        .with_launcher_environment(
            xpack_core::paths::HEALTH_FILE_ENV,
            report.display().to_string(),
        );

    // The lock is released before the application runs. Holding it for the
    // lifetime of a user's program would block every other xpack command for
    // as long as the application stayed open.
    drop(lock);

    let mut child = launch(&request)?;
    let status =
        child.wait().map_err(|e| Error::Launch(format!("waiting for the application: {e}")))?;

    if !on_probation {
        return Ok(exit_code(status.code()));
    }

    // Re-acquire to record the outcome; the application has exited by now.
    let lock = context.lock(&args.application)?;
    let installer = Installer::new(&lock);

    if status.success() || report.exists() {
        installer.commit_health()?;
        xpack_core::errln!("version {version} confirmed healthy");
    } else {
        let reason = format!("exited with status {}", status.code().unwrap_or(-1));
        match installer.record_failure(&reason)? {
            Some(restored) => xpack_core::errln!("rolled back to {restored}"),
            None => xpack_core::errln!("error: no healthy version to roll back to"),
        }
    }
    Ok(exit_code(status.code()))
}

/// Maps the application's exit status to this process's.
///
/// The application's own status is passed through, so a script wrapping
/// `xpack run` sees what it would have seen running the program directly.
fn exit_code(code: Option<i32>) -> ExitCode {
    match code {
        Some(0) => ExitCode::SUCCESS,
        // A status outside a byte cannot be represented, and a signal death
        // has no code at all; both become a generic failure.
        Some(other) => u8::try_from(other).map_or(ExitCode::FAILURE, ExitCode::from),
        None => ExitCode::FAILURE,
    }
}
