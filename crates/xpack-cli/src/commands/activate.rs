//! Making a version active.

use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::{Result, Version};
use xpack_install::{HookContext, Installer};

use super::Context;

/// Arguments for `xpack activate`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Application to change.
    #[arg(value_name = "APPLICATION_ID")]
    pub(crate) application: String,

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
    // A newer version is applied with its update hooks, which Ctrl-C may
    // stop and so undo; an older one undoes the active version by request,
    // with its rollback hooks.
    let engine = super::hook_engine_for(lock.paths());
    let state = lock.load_state()?.value;
    let newer = state.current_version.as_ref().is_some_and(|active| &version > active);
    let cancel = newer.then(super::cancel_on_interrupt);
    let hooks = HookContext {
        engine: engine.as_deref(),
        scope: state.scope,
        progress: &crate::progress::HookLines,
        cancel: cancel.as_deref(),
    };
    installer.activate_with_hooks(&hooks, &version, args.allow_downgrade)?;

    crate::output::field("active", &version);
    super::success()
}
