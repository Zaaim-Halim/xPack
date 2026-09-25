//! Keeping the log directory from growing without bound.

use std::path::Path;

/// How many log files to keep: the five most recent days an installation
/// logged on.
///
/// Enough to see what happened around a failure someone reports a few days
/// late, and with each file capped at [`crate::MAX_LOG_FILE_BYTES`] the whole
/// directory stays under about 80 MiB. An installation used every day for
/// three years would otherwise leave a thousand files behind: a new file each
/// day is not enough, something has to end the old ones.
pub const MAX_LOG_FILES: usize = 5;

/// Removes the oldest log files beyond [`MAX_LOG_FILES`].
///
/// Best effort. A log directory that cannot be tidied is not a reason to fail
/// whatever the caller was actually doing.
pub(crate) fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    let mut logs: Vec<_> = entries
        .filter_map(std::result::Result::ok)
        .filter(|e| {
            e.file_name().to_string_lossy().starts_with("xpack.log")
                && e.file_type().is_ok_and(|t| t.is_file())
        })
        .collect();

    if logs.len() <= MAX_LOG_FILES {
        return;
    }

    // The rolling appender names files with a date suffix, so sorting by name
    // orders them by age without asking the filesystem for timestamps, which
    // are less reliable across copies and restores.
    logs.sort_by_key(std::fs::DirEntry::file_name);

    let excess = logs.len() - MAX_LOG_FILES;
    for entry in logs.into_iter().take(excess) {
        let _ = std::fs::remove_file(entry.path());
    }
}
