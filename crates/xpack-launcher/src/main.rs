//! The launcher binary.
//!
//! Every argument is passed to the application unchanged. The launcher takes
//! no flags of its own, because an application's own command line must not be
//! constrained by the thing that starts it; its few settings come from the
//! environment instead.

use std::process::ExitCode;

use xpack_launcher::Launcher;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    let launcher = match Launcher::discover() {
        Ok(launcher) => launcher,
        Err(error) => {
            // Logging is not configured yet: without an installation there is
            // nowhere to write, and this failure means there is no
            // installation.
            eprintln!("xpack: {error}");
            return ExitCode::FAILURE;
        }
    };

    // The console stays silent because this process shares a terminal with the
    // application it starts, and the user is reading the application's output,
    // not ours. Everything goes to the file instead — which is the only record
    // that exists when a launcher rolls back an update nobody was watching.
    xpack_log::init(&xpack_log::Config {
        console: xpack_log::Console::Silent,
        file: Some(launcher.paths()),
        format: xpack_log::Format::Text,
    });

    match launcher.launch(&arguments, true) {
        Ok(outcome) => {
            if let Some(target) = &outcome.rolled_back_to {
                eprintln!("xpack: this version failed to start; rolled back to {target}");
                eprintln!("xpack: start the application again to run it");
                return ExitCode::FAILURE;
            }
            match outcome.exit_code {
                Some(0) | None => ExitCode::SUCCESS,
                Some(code) => u8::try_from(code).map_or(ExitCode::FAILURE, ExitCode::from),
            }
        }
        Err(error) => {
            eprintln!("xpack: {error}");
            ExitCode::FAILURE
        }
    }
}
