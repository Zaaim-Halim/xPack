//! A program logging in a loop cannot fill the disk.
//!
//! In its own test binary, because installing a subscriber is a one-time
//! operation per process.

use xpack_core::InstallPaths;
use xpack_log::{Config, Console, Format, MAX_LOG_FILE_BYTES};

#[test]
fn a_program_logging_in_a_loop_stops_at_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let paths = InstallPaths::new(dir.path(), "com.example.app").unwrap();
    let outcome = xpack_log::init(&Config {
        console: Console::Silent,
        file: Some(&paths),
        format: Format::Text,
    });
    assert!(outcome.is_enabled(), "got {outcome:?}");

    // Well past the limit: about 24 MiB of records.
    let filler = "x".repeat(200);
    for i in 0..120_000 {
        tracing::info!(target: "xpack_install", i, filler = %filler, "a runaway loop");
    }

    let files: Vec<_> = std::fs::read_dir(paths.logs_dir())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("xpack.log"))
        .collect();
    assert_eq!(files.len(), 1, "one day, one file");
    // Asked of the file itself. A directory entry's size can lag on Windows
    // while the file is still open for writing, as this one is: the logging
    // set up above keeps it open for the rest of the process.
    let size = std::fs::metadata(files[0].path()).unwrap().len();
    assert!(size >= MAX_LOG_FILE_BYTES, "stopped early, at {size} bytes");
    assert!(size < MAX_LOG_FILE_BYTES + 4096, "grew to {size} bytes, past the limit");

    let text = std::fs::read_to_string(files[0].path()).unwrap();
    assert_eq!(text.matches("reached its 16 MiB limit").count(), 1);
    assert!(text.trim_end().ends_with("tomorrow's file starts empty"));
}
