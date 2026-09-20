//! The uninstaller binary.
//!
//! Ships inside the installation it removes, and relocates itself before doing
//! so. See the crate documentation for why.

use std::process::ExitCode;

use clap::Parser;
use xpack_core::InstallPaths;
use xpack_platform::InstallLock;
use xpack_uninstaller::{Plan, clean_up_relocated_copy, plan, relocate};

#[derive(Parser)]
#[command(name = "xpack-uninstaller", about = "Removes this xPack installation")]
struct Args {
    /// Required, because this cannot be undone.
    #[arg(long)]
    yes: bool,

    /// Installation to remove. Defaults to the one containing this binary.
    #[arg(long, value_name = "DIR")]
    application_dir: Option<std::path::PathBuf>,

    /// Set on the relocated copy. Not for people to pass.
    #[arg(long, hide = true)]
    relocated: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();

    let paths = match resolve(&args) {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("xpack-uninstaller: {error}");
            return ExitCode::FAILURE;
        }
    };

    if !args.yes {
        eprintln!(
            "xpack-uninstaller: this removes every installed version of {}.",
            paths.application_id().unwrap_or("this application")
        );
        eprintln!("Pass --yes to confirm.");
        return ExitCode::FAILURE;
    }

    // Logging into the installation only makes sense before it is removed, and
    // only from the process that is not about to delete the log directory out
    // from under itself.
    if !args.relocated {
        xpack_log::init(&xpack_log::Config {
            console: xpack_log::Console::Silent,
            file: Some(&paths),
            format: xpack_log::Format::Text,
        });
    }

    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("xpack-uninstaller: cannot locate this binary: {error}");
            return ExitCode::FAILURE;
        }
    };

    match plan(&paths, &executable, args.relocated) {
        Ok(Plan::Relocate { to }) => hand_over(&executable, &to, &paths),
        Ok(Plan::RemoveNow) => remove(&paths, args.relocated, &executable),
        Err(error) => {
            eprintln!("xpack-uninstaller: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Copies this binary out of the installation and lets the copy finish the job.
fn hand_over(executable: &std::path::Path, to: &std::path::Path, paths: &InstallPaths) -> ExitCode {
    if let Err(error) = relocate(executable, to) {
        eprintln!("xpack-uninstaller: could not prepare removal: {error}");
        return ExitCode::FAILURE;
    }

    let spawned = std::process::Command::new(to)
        .arg("--yes")
        .arg("--relocated")
        .arg("--application-dir")
        .arg(paths.root())
        .spawn();

    match spawned {
        // Deliberately not waited on. On Windows this process must *exit*
        // before its image is unlocked, and the copy cannot finish until it
        // does; waiting here would deadlock the two against each other.
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xpack-uninstaller: could not start the removal: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Performs the removal.
fn remove(paths: &InstallPaths, relocated: bool, executable: &std::path::Path) -> ExitCode {
    let lock = match InstallLock::acquire(paths) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("xpack-uninstaller: {error}");
            return ExitCode::FAILURE;
        }
    };

    let removal = match xpack_install::uninstall(lock) {
        Ok(removal) => removal,
        Err(error) => {
            eprintln!("xpack-uninstaller: {error}");
            return ExitCode::FAILURE;
        }
    };

    if let xpack_install::DesktopOutcome::Done(entries) = &removal.desktop {
        for entry in entries {
            eprintln!("Removed the desktop entry at {}", entry.display());
        }
    }

    if removal.is_complete() {
        eprintln!("Removed {}", removal.root.display());
    } else {
        eprintln!(
            "Removed everything xPack installed in {}, but the directory was not empty.",
            removal.root.display()
        );
        eprintln!("These were left alone because they are not ours to delete:");
        for path in &removal.remaining {
            eprintln!("  {}", path.display());
        }
    }
    eprintln!(
        "Application data was not touched. xPack does not know where an application stores \
         its data, so it does not guess."
    );

    if relocated {
        clean_up_relocated_copy(executable);
    }
    ExitCode::SUCCESS
}

/// The installation to act on.
fn resolve(args: &Args) -> xpack_core::Result<InstallPaths> {
    match &args.application_dir {
        Some(dir) => Ok(InstallPaths::from_application_dir(dir)),
        None => InstallPaths::discover(),
    }
}
