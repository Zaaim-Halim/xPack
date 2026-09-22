//! The console build of the installer. See the library documentation.

use std::process::ExitCode;

fn main() -> ExitCode {
    xpack_installer_stub::main(xpack_installer_stub::Build::Console)
}
