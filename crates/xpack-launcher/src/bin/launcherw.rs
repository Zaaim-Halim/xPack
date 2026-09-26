//! The launcher binary, linked for the Windows subsystem.
//!
//! The same behaviour as `xpack-launcher`, but for two things that follow from
//! the subsystem, which is a link-time property and cannot be chosen when the
//! program runs: this binary has no console, and it starts a graphical
//! application without one either. A shortcut pointing here opens the user's
//! application with no console window appearing behind it.
//!
//! On Unix the attribute does nothing and this binary is an exact duplicate of
//! the other, so the installer places it only on Windows.

// `windows_subsystem` is accepted on every target but only means something on
// Windows. Applied unconditionally, so the two binaries differ in this
// attribute and in which entry point they call, and in nothing else.
#![windows_subsystem = "windows"]

use std::process::ExitCode;

fn main() -> ExitCode {
    xpack_launcher::run_windowed()
}
