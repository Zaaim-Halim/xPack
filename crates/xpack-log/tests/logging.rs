//! Logging setup, with emphasis on the file that nobody watches.

use std::path::Path;

use xpack_core::InstallPaths;
use xpack_log::{Config, Console, FileLogging, Format, MAX_LOG_FILES};

fn paths(root: &Path) -> InstallPaths {
    InstallPaths::new(root, "com.example.app").unwrap()
}

#[test]
fn a_command_with_no_installation_logs_only_to_the_console() {
    // Building or inspecting a package happens long before an installation
    // exists, so there is nowhere to write and that is not an error.
    let outcome = xpack_log::init(&Config::console_only(Console::Normal));
    assert_eq!(outcome, FileLogging::Disabled);
}

#[test]
fn an_unusable_log_directory_does_not_fail_the_operation() {
    // A log directory that cannot be written is a degraded installation, not a
    // failed one. Refusing to install an application because its log file
    // could not be opened would be a far worse outcome than the missing log.
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());

    // A regular file where the log directory needs to be.
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    std::fs::write(paths.logs_dir(), b"not a directory").unwrap();

    let outcome = xpack_log::init(&Config {
        console: Console::Silent,
        file: Some(&paths),
        format: Format::Text,
    });
    assert!(
        matches!(outcome, FileLogging::Unavailable(_)),
        "must report rather than fail: {outcome:?}"
    );
    assert!(!outcome.is_enabled());
}

#[test]
fn old_log_files_are_pruned_so_the_directory_stays_bounded() {
    // Rotation decides when to start a new file. Something separate has to end
    // the old ones, or an installer running daily for years leaves a thousand.
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());
    let logs = paths.logs_dir();
    std::fs::create_dir_all(&logs).unwrap();

    for day in 0..(MAX_LOG_FILES + 10) {
        std::fs::write(logs.join(format!("xpack.log.2020-01-{day:02}")), b"old").unwrap();
    }
    // Something that is not ours must survive.
    std::fs::write(logs.join("keep-me.txt"), b"unrelated").unwrap();

    xpack_log::init(&Config { console: Console::Silent, file: Some(&paths), format: Format::Text });

    let remaining: Vec<String> = std::fs::read_dir(&logs)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("xpack.log"))
        .collect();
    assert!(
        remaining.len() <= MAX_LOG_FILES + 1,
        "expected pruning to a bound, found {}",
        remaining.len()
    );
    // The oldest go first, so the newest must still be there.
    assert!(
        remaining.iter().any(|n| n.contains("2020-01-23")),
        "the newest files must be kept: {remaining:?}"
    );
    assert!(logs.join("keep-me.txt").exists(), "unrelated files must not be touched");
}

#[test]
fn the_default_filter_names_each_crate_rather_than_matching_a_prefix() {
    // A directive such as `xpack=info` matches on raw string prefix, not on a
    // `::` boundary. It catches xpack_install today by accident and would
    // silently start catching an unrelated `xpackfoo` tomorrow.
    let directives = xpack_log::default_directives(Console::Normal);
    assert!(directives.contains("xpack_install=info"), "{directives}");
    assert!(directives.contains("xpack_launcher=info"), "{directives}");
    assert!(
        !directives.split(',').any(|d| d == "xpack=info"),
        "a bare prefix directive must not be used: {directives}"
    );
}

#[test]
fn initialising_twice_is_harmless() {
    // A test binary and the code under test may both reach it.
    let first = xpack_log::init(&Config::console_only(Console::Quiet));
    let second = xpack_log::init(&Config::console_only(Console::Verbose));
    assert_eq!(first, FileLogging::Disabled);
    assert_eq!(second, FileLogging::Disabled);
}
