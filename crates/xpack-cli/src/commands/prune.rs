//! Removing versions that are no longer needed.

use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::Result;
use xpack_install::Installer;

use super::Context;

/// Arguments for `xpack prune`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Application to prune.
    #[arg(value_name = "APPLICATION_ID")]
    application: String,
}

/// Runs `xpack prune`.
pub(crate) fn run(args: &Args, context: &Context) -> Result<ExitCode> {
    let lock = context.lock(&args.application)?;
    let installer = Installer::new(&lock);
    installer.recover()?;

    let removed = installer.prune()?;
    if removed.is_empty() {
        println!("nothing to remove");
    } else {
        for version in &removed {
            println!("removed {version}");
        }
    }
    super::success()
}
