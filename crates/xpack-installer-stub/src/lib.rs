//! The installer executables every xPack installer is built on.
//!
//! Two roles each. Built by `xpack installer`, one carries a payload and
//! installs an application. Unbuilt, it is the stub that command appends to,
//! and says so rather than failing obscurely.
//!
//! # With a window, or without
//!
//! Which one runs is decided by which executable this is and by its flags,
//! never by guessing whether a person is watching. A guess that is wrong
//! either hangs a script on a window nobody sees or hides the window from the
//! person who double-clicked:
//!
//! | Build | No flag | `--silent` or `--dry-run` |
//! | --- | --- | --- |
//! | `xpack-installerw` (Windows) | wizard | no window |
//! | `xpack-installer` on macOS | wizard | no window |
//! | `xpack-installer` elsewhere | no window | no window |
//!
//! Both paths make the same install call through the same request, so a
//! wizard clicked straight through and a silent install do the same thing.

mod console;
mod wizard;

use std::process::ExitCode;

use clap::Parser;
use xpack_installer::{Payload, VerifiedPayload, bundle, exit_code_for};

/// Which executable this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Build {
    /// The console build.
    Console,
    /// The Windows-subsystem build.
    Windowed,
}

/// Runs the installer.
pub fn main(build: Build) -> ExitCode {
    let args = console::Args::parse();

    let window = opens_a_window(build, &args);
    let log = start_logging(&args, window);

    if window && let Some(code) = wizard::run(&args, log) {
        return code;
    }

    match console::run(&args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("xpack-installer: {error}");
            ExitCode::from(exit_code_for(&error))
        }
    }
}

/// Starts logging, and returns the file it goes to, when there is one.
///
/// There is no installation to log into yet — that is the point of this
/// binary — so a file is used only when asked for, or when a window means
/// nobody will see the console.
fn start_logging(args: &console::Args, window: bool) -> Option<std::path::PathBuf> {
    let console =
        if args.verbose { xpack_log::Console::Verbose } else { xpack_log::Console::Normal };
    let file = args
        .log
        .clone()
        .or_else(|| window.then(|| std::env::temp_dir().join("xpack-installer.log")));
    let Some(file) = file else {
        xpack_log::init(&xpack_log::Config::console_only(console));
        return None;
    };
    match xpack_log::init_with_file(console, &file, xpack_log::Format::Text) {
        xpack_log::FileLogging::Enabled(path) => Some(path),
        _ => None,
    }
}

/// Whether this run opens the wizard. See the crate documentation.
fn opens_a_window(build: Build, args: &console::Args) -> bool {
    if args.silent || args.dry_run {
        return false;
    }
    match build {
        Build::Windowed => true,
        Build::Console => cfg!(target_os = "macos"),
    }
}

/// Finds this installer's payload, unpacks it and verifies its package.
///
/// Nothing is shown and nothing is installed from a payload that has not
/// passed through here.
fn load() -> xpack_core::Result<VerifiedPayload> {
    let (executable, source) = locate()?;
    let bytes = bundle::read(&executable, &source)?;
    Payload::unpack(&bytes)?.verify()
}

/// Finds where this installer's payload is, without reading it.
///
/// Failing here means an unbuilt stub, which only a developer ever runs: it
/// is told so on the console, never in a window.
fn locate() -> xpack_core::Result<(std::path::PathBuf, bundle::Source)> {
    let executable = std::env::current_exe().map_err(|e| {
        xpack_core::Error::invalid("installer", format!("cannot locate this binary: {e}"))
    })?;
    let source = bundle::locate(&executable)?;
    Ok((executable, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(flags: &[&str]) -> console::Args {
        console::Args::parse_from(std::iter::once("xpack-installer").chain(flags.iter().copied()))
    }

    #[test]
    fn the_windowed_build_opens_the_wizard_unless_told_not_to() {
        assert!(opens_a_window(Build::Windowed, &args(&[])));
        assert!(!opens_a_window(Build::Windowed, &args(&["--silent"])));
        assert!(!opens_a_window(Build::Windowed, &args(&["--dry-run"])));
    }

    #[test]
    fn the_console_build_opens_the_wizard_only_on_macos() {
        // There the console build is what the installer bundle runs, so it is
        // what a person double-clicks.
        assert_eq!(opens_a_window(Build::Console, &args(&[])), cfg!(target_os = "macos"));
        assert!(!opens_a_window(Build::Console, &args(&["--silent"])));
    }
}
