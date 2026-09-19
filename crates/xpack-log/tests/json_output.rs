//! That JSON output is genuinely parseable JSON.
//!
//! In its own test binary, because installing a subscriber is a one-time
//! operation per process and this one needs a specific configuration.

use std::path::Path;

use xpack_core::InstallPaths;
use xpack_log::{Config, Console, Format};

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
fn json_records_are_one_parseable_object_per_line() {
    // A log meant for collection has to survive a parser, not merely look
    // structured. Asserting the configuration was accepted proves nothing.
    let dir = tempfile::tempdir().unwrap();
    let paths = InstallPaths::new(dir.path(), "com.example.app").unwrap();

    let outcome = xpack_log::init(&Config {
        console: Console::Silent,
        file: Some(&paths),
        format: Format::Json,
    });
    assert!(outcome.is_enabled(), "got {outcome:?}");

    tracing::info!(target: "xpack_install", version = "1.2.0", "version installed");
    tracing::warn!(target: "xpack_update", reason = "checksum", "download rejected");

    let contents = log_contents(&paths.logs_dir());
    let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "one object per record: {contents:?}");

    for line in &lines {
        let parsed: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"));
        assert!(parsed.get("level").is_some(), "a record must carry its level: {line}");
        assert!(parsed.get("fields").is_some(), "a record must carry its fields: {line}");
    }

    assert!(contents.contains("1.2.0"), "structured fields must survive: {contents}");
}
