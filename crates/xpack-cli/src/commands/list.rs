//! Listing installed versions.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args as ClapArgs;
use serde::Serialize;
use xpack_core::Result;
use xpack_install::Installer;

use super::Context;

/// Arguments for `xpack list`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Application to list.
    #[arg(value_name = "APPLICATION_ID")]
    pub(crate) application: String,

    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

/// One row of the listing.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    version: String,
    status: String,
    active: bool,
    /// Whether the files are actually present, which state alone cannot say.
    present: bool,
}

/// What `xpack list --json` prints.
///
/// An object rather than the bare array of versions it used to be, because
/// the executables an installation holds are named after the application it
/// serves and there is otherwise no way to find out what they are called. A
/// caller that reconstructed those names would be a second implementation of
/// a rule that has to stay in one place, and would be wrong for every
/// installation made before the rule existed.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Listing<'a> {
    application: &'a str,
    root: &'a Path,
    /// The executable that starts the application.
    ///
    /// On Windows this is the windowed build, which is what a shortcut points
    /// at; elsewhere there is only one launcher and this is it.
    ///
    /// Null when nothing is installed at that path. Reporting a path that is
    /// not there would hand a caller something to run that does not exist.
    launcher: Option<PathBuf>,
    /// The console build, which differs from `launcher` only on Windows.
    console_launcher: Option<PathBuf>,
    updater: Option<PathBuf>,
    uninstaller: Option<PathBuf>,
    versions: Vec<Entry>,
}

/// A path, reported only when something is actually there.
fn if_present(path: PathBuf) -> Option<PathBuf> {
    path.is_file().then_some(path)
}

/// Runs `xpack list`.
pub(crate) fn run(args: &Args, context: &Context) -> Result<ExitCode> {
    let lock = context.lock(&args.application)?;
    let installer = Installer::new(&lock);
    let state = lock.load_or_new_state(&args.application)?;
    let active = state.current_version.clone();

    let entries: Vec<Entry> = installer
        .installed()?
        .into_iter()
        .map(|(version, status)| Entry {
            active: active.as_ref() == Some(&version),
            present: installer.is_usable(&version),
            version: version.to_string(),
            status: format!("{status:?}").to_lowercase(),
        })
        .collect();

    if args.json {
        let paths = lock.paths();
        let names = state.binary_names();
        let listing = Listing {
            application: &args.application,
            root: paths.root(),
            launcher: if_present(paths.shortcut_target_named(&names)),
            console_launcher: if_present(paths.launcher_file_named(&names)),
            updater: if_present(paths.updater_file_named(&names)),
            uninstaller: if_present(paths.uninstaller_file_named(&names)),
            versions: entries,
        };
        return crate::output::json(&listing).map(|()| ExitCode::SUCCESS);
    }

    if entries.is_empty() {
        xpack_core::outln!("no versions installed");
        return super::success();
    }

    for entry in &entries {
        let marker = if entry.active { "*" } else { " " };
        let missing = if entry.present { "" } else { "   (files missing)" };
        xpack_core::outln!("{marker} {:<16} {}{missing}", entry.version, entry.status);
    }

    if !state.update.is_idle() {
        xpack_core::errln!();
        xpack_core::errln!("note: {}", state.update.describe());
    }
    super::success()
}
