//! Returning to the previous healthy version.

use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::Result;
use xpack_install::Installer;

use super::Context;

/// Arguments for `xpack rollback`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Application to roll back.
    #[arg(value_name = "APPLICATION_ID")]
    pub(crate) application: String,
}

/// Runs `xpack rollback`.
pub(crate) fn run(args: &Args, context: &Context) -> Result<ExitCode> {
    let lock = context.lock(&args.application)?;
    let installer = Installer::new(&lock);
    installer.recover()?;

    if let Some(version) = installer.rollback()? {
        crate::output::field("active", &version);
        return super::success();
    }
    // Not an error in the sense of something going wrong, but nothing
    // succeeded either, so the exit code must not report success.
    xpack_core::errln!("error: no healthy version to roll back to");
    xpack_core::errln!("install a working version to recover this installation");
    Ok(ExitCode::FAILURE)
}
