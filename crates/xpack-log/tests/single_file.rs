//! Logging into one named file, for a run with no installation yet.
//!
//! In its own test binary because a process installs one subscriber, once.

use xpack_log::{Console, FileLogging, Format};

#[test]
fn records_reach_the_named_file_and_a_later_run_appends() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("nested/xpack-installer.log");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "an earlier attempt\n").unwrap();

    let outcome = xpack_log::init_with_file(Console::Silent, &file, Format::Text);
    assert_eq!(outcome, FileLogging::Enabled(file.clone()));

    tracing::info!(target: "xpack_installer", "the installer started");

    let written = std::fs::read_to_string(&file).unwrap();
    assert!(written.starts_with("an earlier attempt\n"), "the earlier record was lost: {written}");
    assert!(written.contains("the installer started"), "{written}");
}
