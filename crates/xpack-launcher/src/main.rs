//! The launcher binary.
//!
//! Every argument is passed to the application unchanged. The launcher takes
//! no flags of its own, because an application's own command line must not be
//! constrained by the thing that starts it; its few settings come from the
//! environment instead.

use std::process::ExitCode;

use xpack_launcher::Launcher;

fn main() -> ExitCode {
    init_logging();

    let arguments: Vec<String> = std::env::args().skip(1).collect();

    let launcher = match Launcher::discover() {
        Ok(launcher) => launcher,
        Err(error) => {
            eprintln!("xpack: {error}");
            return ExitCode::FAILURE;
        }
    };

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

/// Logs to standard error, quiet unless something goes wrong.
///
/// The launcher shares a terminal with the application it starts, so it must
/// not add noise to output the user is reading.
fn init_logging() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_env("XPACK_LOG")
        .unwrap_or_else(|_| EnvFilter::new("xpack_launcher=warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .without_time()
        .with_target(false)
        .try_init();
}
