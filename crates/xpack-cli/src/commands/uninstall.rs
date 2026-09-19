//! Removing an installation.

use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::Result;
use xpack_install::Installer;

use super::Context;

/// Arguments for `xpack uninstall`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Application to remove.
    #[arg(value_name = "APPLICATION_ID")]
    application: String,

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

    Installer::new(&lock).uninstall()?;
    crate::output::field("removed", lock.paths().root().display());
    eprintln!(
        "note: application data was not touched. xPack does not know where an application \
         stores its data, so it does not guess."
    );
    super::success()
}
