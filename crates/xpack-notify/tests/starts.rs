//! The notice starts, as it was built.
//!
//! On Windows it opens a window, and a program that does needs the
//! application manifest linked in: without it Windows refuses to start it at
//! all, before a line of it runs, with "entry point not found". An installation
//! made without an installer carries this executable exactly as it was built,
//! so it has to start that way.
//!
//! `--help` shows no dialog, so this runs on any machine, with or without a
//! display, and still has to get as far as running the program.

#[test]
fn the_notice_starts_as_it_was_built() {
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_xpack-notify"))
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0), "{status:?}");
}
