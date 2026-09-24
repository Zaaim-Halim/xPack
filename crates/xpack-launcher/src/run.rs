//! The body both launcher binaries share.
//!
//! Two executables are installed on Windows and they differ in exactly one
//! way: the subsystem they are linked for. Everything they *do* is here, so
//! the console build and the windowed build cannot drift apart.
//!
//! # Why two binaries
//!
//! A Windows executable declares at link time whether it is a console program
//! or a windowed one, and the choice cannot be made at runtime. A console
//! build opened from a Start-Menu shortcut gets a console allocated for it,
//! and an empty black window sits behind the user's application for as long as
//! it runs. A windowed build never gets one — but it also has no standard
//! output, so a user who runs it from a terminal sees nothing at all.
//!
//! Neither is right for both uses, so both are shipped: shortcuts point at the
//! windowed build, terminals and scripts keep the console one. On Unix the
//! distinction does not exist and only the one binary is installed.
//!
//! # Nothing here may use `println!` or `eprintln!`
//!
//! In a windowed build there is no standard error to write to.
//! `GetStdHandle` returns null, the write fails, and `eprintln!` **panics** on
//! a failed write — which, with `panic = "abort"` in the release profile, ends
//! the process. A launcher that aborts because it tried to report an error is
//! a launcher that loses the user's application.
//!
//! So every message goes through [`xpack_core::errln!`], which discards the
//! error. On a console build it prints; on a windowed build it silently does
//! nothing and the log file carries the record instead.

use std::process::ExitCode;

use crate::Launcher;

/// Starts the application, and starts it once more if the user agreed to.
///
/// The second start is what makes "restart now" mean anything: the update
/// prompt asked the application to close, the user let it, and this brings it
/// back — activating the staged version on the way, because that is what the
/// first thing any launch does.
///
/// # Exactly once
///
/// A loop here would be a way to lose a user's machine to their own updater. A
/// version that crashes on start, or a prompt that somehow fires again, would
/// have the launcher restarting the application for as long as anyone let it.
/// One restart per launcher process is all this offers: a user who wants
/// another can ask for one, and the rollback that watches every activation is
/// still the thing protecting them from a version that will not start.
fn launch_with_one_restart(
    launcher: &Launcher,
    arguments: &[String],
) -> xpack_core::Result<crate::Outcome> {
    let outcome = launcher.launch(arguments, true)?;
    if !outcome.restart_requested {
        return Ok(outcome);
    }

    tracing::info!("the user agreed to restart; starting the updated version");

    // The second launch offers no further restart. This process performs one,
    // so a prompt during that second run would ask the application to close
    // and leave nothing to bring it back — the user's application would simply
    // disappear. It still checks, stages and announces; the wording becomes
    // "at the next start", which by then is the truth.
    Launcher::for_application_dir(launcher.paths().root())
        .without_restart_offers()
        .launch(arguments, true)
}

/// Runs a launcher binary from start to finish.
///
/// Returns the exit code the process should end with, rather than exiting
/// itself, so that both `main` functions stay one line long and this stays
/// testable.
pub fn run() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    let launcher = match Launcher::discover() {
        Ok(launcher) => launcher,
        Err(error) => {
            // Logging is not configured yet: without an installation there is
            // nowhere to write, and this failure means there is no
            // installation. On a windowed build this line goes nowhere, which
            // is the accepted cost of having no console — the alternative is a
            // message box, which needs Win32 calls this workspace forbids.
            xpack_core::errln!("xpack: {error}");
            return ExitCode::FAILURE;
        }
    };

    // The console stays silent because this process shares a terminal with the
    // application it starts, and the user is reading the application's output,
    // not ours. Everything goes to the file instead — which is the only record
    // that exists when a launcher rolls back an update nobody was watching,
    // and on a windowed build it is the only record that can exist at all.
    xpack_log::init(&xpack_log::Config {
        console: xpack_log::Console::Silent,
        file: Some(launcher.paths()),
        format: xpack_log::Format::Text,
    });

    // One of the application's extra commands runs its own program, and
    // nothing else of an ordinary start applies to it.
    let executable = std::env::current_exe().unwrap_or_default();
    let requested = crate::launcher::requested_command(
        &executable,
        std::env::var_os(xpack_core::paths::COMMAND_ENV),
    );
    if let Some(name) = requested {
        match launcher.run_command(&name, &arguments) {
            Ok(Some(code)) => {
                return match code {
                    Some(0) => ExitCode::SUCCESS,
                    Some(code) => u8::try_from(code).map_or(ExitCode::FAILURE, ExitCode::from),
                    None => ExitCode::FAILURE,
                };
            }
            Ok(None) => {}
            Err(error) => {
                tracing::error!(%error, command = %name, "the command could not be started");
                xpack_core::errln!("{name}: {error}");
                return ExitCode::FAILURE;
            }
        }
    }

    // Started before the application, so a slow network never delays opening
    // it, and deliberately not waited on. Whatever it finds takes effect the
    // next time the application starts.
    crate::spawn_updater(launcher.paths());

    match launch_with_one_restart(&launcher, &arguments) {
        Ok(outcome) => {
            if let Some(target) = &outcome.rolled_back_to {
                // Logged as well as printed, because a windowed build has
                // nowhere to print and this is the one message a user most
                // needs to be able to find afterwards.
                tracing::warn!(%target, "this version failed to start; rolled back");
                xpack_core::errln!("xpack: this version failed to start; rolled back to {target}");
                xpack_core::errln!("xpack: start the application again to run it");
                return ExitCode::FAILURE;
            }
            match outcome.exit_code {
                Some(0) | None => ExitCode::SUCCESS,
                Some(code) => u8::try_from(code).map_or(ExitCode::FAILURE, ExitCode::from),
            }
        }
        Err(error) => {
            tracing::error!(%error, "the application could not be started");
            xpack_core::errln!("xpack: {error}");
            ExitCode::FAILURE
        }
    }
}
