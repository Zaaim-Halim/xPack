//! Checking for and applying updates.

use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::{Error, Result};
use xpack_update::{HttpsTransport, UpdateOptions, Updater};

use super::Context;
use crate::progress::TerminalProgress;

/// Arguments for `xpack update`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Application to update.
    #[arg(value_name = "APPLICATION_ID")]
    pub(crate) application: String,

    /// Base URL of the update server. Defaults to the installed manifest's.
    #[arg(long, value_name = "URL")]
    url: Option<String>,

    /// Report what is available without downloading anything.
    #[arg(long)]
    check_only: bool,

    /// Do not activate the new version once installed.
    #[arg(long)]
    no_activate: bool,

    /// Permit moving to a version that is not newer.
    #[arg(long)]
    allow_downgrade: bool,
}

/// Runs `xpack update`.
pub(crate) fn run(args: &Args, context: &Context) -> Result<ExitCode> {
    let lock = context.lock(&args.application)?;
    let url = resolve_url(args, &lock)?;

    let transport = HttpsTransport::new();
    let progress = TerminalProgress::new();
    let updater = Updater::new(&lock, &transport).reporting_to(&progress);

    let options =
        UpdateOptions { allow_downgrade: args.allow_downgrade, activate: !args.no_activate };

    if args.check_only {
        let outcome = updater.check(&url, &options);
        progress.finish();
        if let Some(available) = outcome? {
            crate::output::field("available", &available.version);
            if let Some(current) = &available.current {
                crate::output::field("current", current);
            }
            crate::output::field(
                "size",
                crate::commands::pack::format_size(available.declared_size),
            );
            if let Some(notes) = &available.release_notes {
                crate::output::field("notes", notes);
            }
        } else {
            println!("up to date");
        }
        return super::success();
    }

    let outcome = updater.update(&url, &options);
    // The bar is cleared before anything is printed or reported, so a failure
    // message is never interleaved with a half-drawn line.
    progress.finish();

    if let Some(version) = outcome? {
        crate::output::field("updated to", &version);
    } else {
        println!("up to date");
    }
    super::success()
}

/// Finds the update server for this installation.
///
/// The installed version's own manifest carries it, so the server is whatever
/// the signed package said it was rather than whatever a caller happened to
/// type. `--url` overrides it, which is what a private mirror or a test needs.
fn resolve_url(args: &Args, lock: &xpack_platform::InstallLock) -> Result<String> {
    if let Some(url) = &args.url {
        return Ok(url.clone());
    }

    let state = lock.load_or_new_state(&args.application)?;
    let version = state.active()?;
    let manifest_file = lock.paths().version_manifest_file(version);
    let bytes = std::fs::read(&manifest_file).map_err(|e| Error::io(&manifest_file, e))?;
    let manifest = xpack_core::Manifest::from_slice(&bytes)?;

    manifest.update.url.clone().ok_or_else(|| {
        Error::invalid(
            "update",
            "the installed version declares no update server; pass --url to name one",
        )
    })
}
