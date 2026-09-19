//! Checking for and applying updates.

use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::{Error, Result};
use xpack_update::{HttpsTransport, Timeouts, UpdateOptions, Updater};

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

    /// Give up after this many minutes.
    ///
    /// Bounds the whole operation. The default is generous because a large
    /// package on a slow connection legitimately takes a long time; a server
    /// that stalls is caught much sooner by the narrower transport timeouts.
    #[arg(long, value_name = "MINUTES", default_value_t = 30)]
    timeout: u64,
}

/// Runs `xpack update`.
pub(crate) fn run(args: &Args, context: &Context) -> Result<ExitCode> {
    // Deliberately no lock here. The updater takes it in short windows of its
    // own and releases it across the download, so that a user can still start
    // their application while an update is being fetched. Holding one here
    // would deadlock it against itself on the first window.
    let paths = context.paths(&args.application)?;
    let url = resolve_url(args, &paths)?;

    let timeouts = Timeouts::with_total(std::time::Duration::from_secs(args.timeout * 60))?;
    let transport = HttpsTransport::with_timeouts(timeouts);
    let progress = TerminalProgress::new();
    let updater = Updater::new(&paths, &transport).reporting_to(&progress);

    let options = UpdateOptions {
        allow_downgrade: args.allow_downgrade,
        activate: !args.no_activate,
        launcher: super::default_launcher(),
        updater: super::default_updater(),
        uninstaller: super::default_uninstaller(),
    };

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
fn resolve_url(args: &Args, paths: &xpack_core::InstallPaths) -> Result<String> {
    if let Some(url) = &args.url {
        return Ok(url.clone());
    }

    // Reads state without the lock. That is safe for this one purpose: the
    // value is a URL used to ask a server what it has, and every decision that
    // follows is re-made under the lock by the updater itself. A URL that was
    // correct a moment ago is not a hazard; acting on stale *state* would be,
    // and nothing here does.
    let lock = xpack_platform::InstallLock::acquire(paths)?;
    let state = lock.load_or_new_state(&args.application)?;
    let version = state.active()?;
    let manifest_file = paths.version_manifest_file(version);
    let bytes = std::fs::read(&manifest_file).map_err(|e| Error::io(&manifest_file, e))?;
    let manifest = xpack_core::Manifest::from_slice(&bytes)?;

    manifest.update.url.clone().ok_or_else(|| {
        Error::invalid(
            "update",
            "the installed version declares no update server; pass --url to name one",
        )
    })
}
