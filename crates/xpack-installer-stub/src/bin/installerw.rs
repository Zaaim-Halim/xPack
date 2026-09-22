//! The installer, linked for the Windows subsystem.
//!
//! The same behaviour as `xpack-installer`; only the subsystem differs, which
//! is a link-time property and cannot be chosen when the program runs. Double-
//! clicked, it opens the wizard with no console window behind it.
//!
//! The price is that a shell does not wait for it: `cmd` and PowerShell return
//! before it finishes, and its exit code is lost unless the caller waits
//! explicitly. Scripts use the console build.

// Accepted on every target, meaningful only on Windows. Applied
// unconditionally so the two builds differ in this attribute and nothing else.
#![windows_subsystem = "windows"]

use std::process::ExitCode;

fn main() -> ExitCode {
    xpack_installer_stub::main(xpack_installer_stub::Build::Windowed)
}
