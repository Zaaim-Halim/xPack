//! Whether this process is an Intel build running under Rosetta on an Apple
//! Silicon Mac.
//!
//! Such a process believes it runs on an Intel Mac, so everything that asks
//! which platform it is on gets the answer the build was compiled for. An
//! Intel installer run on an Apple Silicon Mac then takes the Intel package to
//! be the right one for the machine. Asked of the system itself, through
//! `sysctl`, which answers for the process that runs it: a child of a
//! translated process is translated too.

/// Whether this process runs translated by Rosetta. Never true off macOS.
pub fn translated_by_rosetta() -> bool {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", "sysctl.proc_translated"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "1")
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_intel_build_on_apple_silicon_knows_it_is_translated() {
        // Run by building the suite for Intel on an Apple Silicon Mac, as
        // `cargo test --target x86_64-apple-darwin` does; skipped otherwise.
        let apple_silicon = std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", "hw.optional.arm64"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1");
        if cfg!(all(target_os = "macos", target_arch = "x86_64")) && apple_silicon {
            assert!(translated_by_rosetta());
        }
    }

    #[test]
    fn a_native_build_is_not_translated() {
        // This test binary is built for the machine it runs on, unless the
        // suite itself was built for Intel on Apple Silicon.
        if cfg!(target_arch = "aarch64") || !cfg!(target_os = "macos") {
            assert!(!translated_by_rosetta());
        }
    }
}
