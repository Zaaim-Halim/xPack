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
    application: String,
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
    eprintln!("error: no healthy version to roll back to");
    eprintln!("install a working version to recover this installation");
    Ok(ExitCode::FAILURE)
}
