//! That a record actually reaches the file.
//!
//! In its own test binary on purpose. Installing a subscriber is a one-time
//! operation per process, so a test that needs a real one cannot share a
//! binary with tests that install a different configuration first.

use std::path::Path;

use xpack_core::InstallPaths;
use xpack_log::{Config, Console, Format};

fn paths(root: &Path) -> InstallPaths {
    InstallPaths::new(root, "com.example.app").unwrap()
}

/// Reads whatever the rolling appender wrote, across any date suffix.
fn log_contents(dir: &Path) -> String {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(std::result::Result::ok)
                .filter(|e| e.file_name().to_string_lossy().starts_with("xpack.log"))
                .filter_map(|e| std::fs::read_to_string(e.path()).ok())
                .collect::<String>()
        })
        .unwrap_or_default()
}

#[test]
fn a_record_reaches_the_file_and_survives_a_normal_exit() {
    // The whole point of this crate. A non-blocking writer would drop records
    // when its guard is dropped early, which is why a blocking one is used —
    // and this is the test that would catch it changing.
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());

    let outcome = xpack_log::init(&Config {
        console: Console::Silent,
        file: Some(&paths),
        format: Format::Text,
    });
    assert!(outcome.is_enabled(), "got {outcome:?}");

    // Emitted under an xPack target, because the filter admits this project's
    // crates and nothing else — dependency noise in an installation log would
    // bury the few lines that matter.
    tracing::info!(target: "xpack_install", marker = "written-to-file", "a durable record");

    let contents = log_contents(&paths.logs_dir());
    assert!(
        contents.contains("written-to-file"),
        "the record must be on disk once the call returns: {contents:?}"
    );
}
