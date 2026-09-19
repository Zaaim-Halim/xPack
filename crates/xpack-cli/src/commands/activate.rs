//! Making a version active.

use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::{Result, Version};
use xpack_install::Installer;

use super::Context;

/// Arguments for `xpack activate`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Application to change.
    #[arg(value_name = "APPLICATION_ID")]
    application: String,

    /// Version to make active.
    #[arg(value_name = "VERSION")]
    version: String,

    /// Permit moving to a version that is not newer.
    #[arg(long)]
    allow_downgrade: bool,
}

/// Runs `xpack activate`.
pub(crate) fn run(args: &Args, context: &Context) -> Result<ExitCode> {
    let version = Version::parse(&args.version)?;
    let lock = context.lock(&args.application)?;
    let installer = Installer::new(&lock);

    installer.recover()?;
    installer.activate(&version, args.allow_downgrade)?;

    crate::output::field("active", &version);
    super::success()
}
