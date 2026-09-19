//! Finishing or undoing an interrupted operation.

use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::Result;
use xpack_install::Installer;

use super::Context;

/// Arguments for `xpack recover`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Application to recover.
    #[arg(value_name = "APPLICATION_ID")]
    pub(crate) application: String,
}

/// Runs `xpack recover`.
///
/// Every other command recovers first, so this exists mainly for diagnosis:
/// it reports exactly what was left behind and what was done about it.
pub(crate) fn run(args: &Args, context: &Context) -> Result<ExitCode> {
    let lock = context.lock(&args.application)?;
    let report = Installer::new(&lock).recover()?;

    if report.is_empty() {
        println!("nothing to recover");
        return super::success();
    }
    if report.cleared_downloads {
        println!("discarded partial downloads");
    }
    for version in &report.removed_staging {
        println!("discarded staging for {version}");
    }
    for version in &report.removed_debris {
        println!("removed incomplete version {version}");
    }
    if let Some(version) = &report.completed_rollback {
        println!("completed rollback to {version}");
    }
    if report.left_in_place {
        println!("left an in-progress operation untouched");
    }
    super::success()
}
