//! The launcher binary, linked for the Windows subsystem.
//!
//! Byte-for-byte the same behaviour as `xpack-launcher`; the only difference
//! is the subsystem, which is a link-time property and cannot be chosen when
//! the program runs. A shortcut pointing here opens the user's application
//! without a console window appearing behind it.
//!
//! On Unix the attribute does nothing and this binary is an exact duplicate of
//! the other, so the installer places it only on Windows.

// `windows_subsystem` is accepted on every target but only means something on
// Windows. Applied unconditionally so that the two binaries differ in one
// attribute and nothing else.
#![windows_subsystem = "windows"]

use std::process::ExitCode;

fn main() -> ExitCode {
    xpack_launcher::run()
}
