//! Returning to the previous healthy version.

use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::Result;
use xpack_install::{HookContext, Installer};

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

    // By request: the version is not marked bad, and its rollback hooks run
    // with cause `requested`. They are what undoes it, so nothing cancels
    // them.
    let engine = super::hook_engine_for(lock.paths());
    let scope = lock.load_state()?.value.scope;
    let hooks = HookContext {
        engine: engine.as_deref(),
        scope,
        progress: &crate::progress::HookLines,
        cancel: None,
    };
    if let Some(version) = installer.roll_back_on_request(&hooks)? {
        crate::output::field("active", &version);
        return super::success();
    }
    // Not an error in the sense of something going wrong, but nothing
    // succeeded either, so the exit code must not report success.
    xpack_core::errln!("error: no healthy version to roll back to");
    xpack_core::errln!("install a working version to recover this installation");
    Ok(ExitCode::FAILURE)
}
