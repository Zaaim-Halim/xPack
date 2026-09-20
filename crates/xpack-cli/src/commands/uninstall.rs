//! Removing an installation.

use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::Result;

use super::Context;

/// Arguments for `xpack uninstall`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Application to remove.
    #[arg(value_name = "APPLICATION_ID")]
    pub(crate) application: String,

    /// Required, because this cannot be undone.
    #[arg(long)]
    yes: bool,
}

/// Runs `xpack uninstall`.
pub(crate) fn run(args: &Args, context: &Context) -> Result<ExitCode> {
    let lock = context.lock(&args.application)?;

    if !args.yes {
        return Err(xpack_core::Error::invalid(
            "uninstall",
            format!(
                "this removes every installed version of {}. Pass --yes to confirm",
                args.application
            ),
        ));
    }

    // Consumes the lock: the lock file sits inside the tree being removed, so
    // it has to be released partway through.
    let removal = xpack_install::uninstall(lock)?;

    crate::output::field("application", &args.application);
    crate::output::field("root", removal.root.display());
    crate::output::field("removed", removal.is_complete());
    if let xpack_install::DesktopOutcome::Done(entries) = &removal.desktop {
        for entry in entries {
            crate::output::field("desktop entry removed", entry.display());
        }
    }

    if !removal.is_complete() {
        eprintln!();
        eprintln!(
            "note: {} was not empty, so it was left in place. Everything xPack installed \
             there is gone; these are not ours to delete:",
            removal.root.display()
        );
        for path in &removal.remaining {
            eprintln!("  {}", path.display());
        }
    }

    eprintln!(
        "note: application data was not touched. xPack does not know where an application \
         stores its data, so it does not guess."
    );
    super::success()
}
