//! A launcher with nowhere to report still exits with its own code.
//!
//! The windowed Windows build has no standard error at all, and a console
//! build can be left writing into a pipe nobody reads. Either way the failure
//! it tried to report must still end the process with that failure's code, not
//! with a crash's.

use std::process::{Command, Stdio};

/// A stream whose reader has gone away: every write to it fails.
fn closed_pipe() -> Stdio {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    writer.into()
}

#[test]
fn a_failure_with_nowhere_to_report_it_keeps_its_exit_code() {
    // An empty directory is no installation, which the launcher reports on
    // standard error before it has anywhere else to write.
    let dir = tempfile::tempdir().unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_xpack-launcher"))
        .env("XPACK_APPLICATION_DIR", dir.path())
        .stdout(Stdio::null())
        .stderr(closed_pipe())
        .status()
        .unwrap();

    assert_eq!(status.code(), Some(1), "{status:?}");
}
