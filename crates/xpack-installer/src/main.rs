//! The installer binary a user runs.
//!
//! Two roles in one file. Built by `xpack installer`, it carries a payload and
//! installs an application. Unbuilt, it is the stub that command appends to,
//! and says so rather than failing obscurely.

use std::process::ExitCode;

use clap::Parser;
use xpack_installer::{Payload, bundle};

/// Exit codes. Anything a script might branch on gets its own.
mod exit {
    /// The application was installed.
    pub(crate) const INSTALLED: u8 = 0;
    /// The installation could not be completed.
    pub(crate) const FAILED: u8 = 1;
    /// A signature or checksum did not verify.
    pub(crate) const INTEGRITY: u8 = 3;
    /// Another xPack operation holds the lock.
    pub(crate) const BUSY: u8 = 4;
}

#[derive(Parser)]
#[command(name = "xpack-installer", version, about = "Installs an xPack application")]
struct Args {
    /// Install under this directory instead of the per-user default.
    ///
    /// The directory holding every xPack application for this user; the
    /// application's own id is appended to it.
    #[arg(long, value_name = "DIR", env = xpack_core::paths::INSTALL_ROOT_ENV)]
    root: Option<std::path::PathBuf>,

    /// Describe what would be installed, without installing it.
    #[arg(long)]
    dry_run: bool,

    /// Print more detail about what is happening.
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();

    let console =
        if args.verbose { xpack_log::Console::Verbose } else { xpack_log::Console::Normal };
    xpack_log::init(&xpack_log::Config {
        console,
        // There is no installation to log into yet — that is the point of this
        // binary — so the console is the only destination available.
        file: None,
        format: xpack_log::Format::Text,
    });

    match run(&args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("xpack-installer: {error}");
            ExitCode::from(exit_code_for(&error))
        }
    }
}

fn run(args: &Args) -> xpack_core::Result<ExitCode> {
    let executable = std::env::current_exe().map_err(|e| {
        xpack_core::Error::invalid("installer", format!("cannot locate this binary: {e}"))
    })?;

    let source = bundle::locate(&executable)?;
    let bytes = bundle::read(&executable, &source)?;
    let payload = Payload::unpack(&bytes)?;
    let plan = payload.plan();

    println!("{} {}", plan.application_name, plan.version);

    let root = match &args.root {
        Some(root) => root.clone(),
        None => xpack_core::paths::default_install_root()?,
    };

    if args.dry_run {
        println!("would install into {}", root.join(&plan.application_id).display());
        println!("signed by       {}", short_key(&plan.signing_key));
        return Ok(ExitCode::from(exit::INSTALLED));
    }

    let outcome = payload.install_into(&root)?;

    println!();
    println!("Installed into {}", outcome.root.display());
    if let xpack_install::DesktopOutcome::Done(entries) = &outcome.desktop {
        for entry in entries {
            println!("Added         {}", entry.display());
        }
    }
    println!();
    println!("Run it with:   {}", outcome.launcher.display());

    Ok(ExitCode::from(exit::INSTALLED))
}

/// A short, readable form of the pinned key, for the person installing.
///
/// The full 64 characters are unreadable and nobody compares them by eye. The
/// leading group is enough to tell two publishers apart when someone has been
/// told what to expect.
fn short_key(hex: &str) -> String {
    hex.chars().take(16).collect::<String>()
}

/// Maps a failure to the exit code a script should branch on.
///
/// Uses the same predicate the CLI does rather than matching variants here, so
/// the two cannot drift into disagreeing about what counts as a security
/// failure — the one code a script must never retry.
fn exit_code_for(error: &xpack_core::Error) -> u8 {
    if error.is_integrity_failure() {
        return exit::INTEGRITY;
    }
    match error {
        xpack_core::Error::Locked(_) => exit::BUSY,
        _ => exit::FAILED,
    }
}
