//! Listing installed versions.

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
    application: String,

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
        return crate::output::json(&entries).map(|()| ExitCode::SUCCESS);
    }

    if entries.is_empty() {
        println!("no versions installed");
        return super::success();
    }

    for entry in &entries {
        let marker = if entry.active { "*" } else { " " };
        let missing = if entry.present { "" } else { "   (files missing)" };
        println!("{marker} {:<16} {}{missing}", entry.version, entry.status);
    }

    if !state.update.is_idle() {
        eprintln!();
        eprintln!("note: {}", state.update.describe());
    }
    super::success()
}
