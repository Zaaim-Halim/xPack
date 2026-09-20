//! The launcher binary, linked for the console subsystem.
//!
//! Every argument is passed to the application unchanged. The launcher takes
//! no flags of its own, because an application's own command line must not be
//! constrained by the thing that starts it; its few settings come from the
//! environment instead.
//!
//! This is the build for terminals and scripts: it keeps standard input,
//! output and error, so a console payload behaves exactly as if the user had
//! run it directly. `xpack-launcherw` is the same program linked for the
//! windowed subsystem, and is what a desktop shortcut should point at.

use std::process::ExitCode;

fn main() -> ExitCode {
    xpack_launcher::run()
}
