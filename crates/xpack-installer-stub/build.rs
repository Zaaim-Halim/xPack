//! Links the Windows application manifest into both installer executables.
//!
//! Without it Windows refuses to start them: the windowing code imports
//! functions that only the Common Controls 6 library has, and only a manifest
//! asks Windows for that version. `xpack installer` writes the same manifest
//! again when it brands an installer; this makes the executable start as it
//! was built, before or without that.
//!
//! The Microsoft linker embeds it, so no resource compiler is needed. Other
//! targets, and a build that only checks the code, never link and are left
//! alone.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../xpack-platform/windows.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());

    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows && msvc {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
        // The manifest declares its own execution level; the linker's default
        // fragment would be a second `trustInfo` beside it.
        println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:NO");
    }
}
