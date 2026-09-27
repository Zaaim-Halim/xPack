//! One running copy of an application, for a package that asks for it.
//!
//! Every start of the application takes the instance lock if it is free, and
//! the launcher holding it keeps it for as long as the application runs. A
//! start that finds it taken knows a copy is running. When the package asks for
//! a single instance, that start hands over instead of starting another copy:
//! it leaves its arguments where the running copy can read them, brings that
//! copy to the front where the platform lets it, and exits.
//!
//! A package that does not ask is unaffected: its starts take the lock when it
//! is free and carry on regardless when it is not.
//!
//! # Bringing the running copy to the front
//!
//! - macOS: opening the application's bundle brings it to the front, and is
//!   what a second click in Finder or the Dock already does without reaching
//!   xPack at all. So a second start from the command opens the bundle, but
//!   only when the running copy was itself opened through it. Opening it for a
//!   copy started from a terminal would start a new one.
//! - Windows: the launcher raises the application's window itself.
//! - Linux: there is no portable way, so the application raises itself when it
//!   reads the request.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use xpack_core::{InstallPaths, Manifest};
use xpack_platform::InstanceLock;

/// How long the launch a restart opened waits for the launcher that asked for
/// it to let go of the lock.
const RESTART_WAIT: Duration = Duration::from_secs(5);

/// How long a start keeps asking when the lock file cannot be opened at all.
///
/// Seen on macOS: for a moment after a newly placed launcher first runs, the
/// file is refused with "operation not permitted". A start that gave up at
/// once would take that for "cannot tell" and start a second copy.
const ERROR_RETRY: Duration = Duration::from_secs(1);

/// What the launcher holding the lock records about the copy it started.
#[derive(Debug, Serialize, Deserialize)]
struct Record {
    /// The application's process.
    pid: u32,
    /// The bundle it was opened through, when it was: see the module
    /// documentation for why that decides how to bring it to the front.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bundle: Option<PathBuf>,
}

/// One request left for the running copy.
#[derive(Debug, Serialize)]
struct Request<'a> {
    arguments: &'a [String],
}

/// What a start found.
#[derive(Debug)]
pub(crate) enum Start {
    /// No copy was running, and this start now holds the lock.
    First(InstanceLock),
    /// A copy is running.
    Another,
    /// The lock could not be asked. The start carries on as if it were alone:
    /// refusing to start the application over a lock file is the worse error.
    Unknown,
}

/// Takes the instance lock if it is free.
///
/// The launch a restart opened waits a moment first, for the launcher that
/// asked for it to let go.
pub(crate) fn begin(paths: &InstallPaths, restarted: bool) -> Start {
    let deadline = Instant::now() + ERROR_RETRY;
    loop {
        let taken = if restarted {
            InstanceLock::acquire_within(paths, RESTART_WAIT)
        } else {
            InstanceLock::acquire(paths)
        };
        match taken {
            Ok(Some(lock)) => {
                let since = SystemTime::now();
                // Before anything else: a start arriving from here on must not
                // find the last session's record and act on a process id that
                // may since belong to another program.
                forget(paths);
                clear_inbox_before(paths, since);
                return Start::First(lock);
            }
            Ok(None) => return Start::Another,
            Err(error) if Instant::now() >= deadline => {
                tracing::warn!(%error, "could not ask whether the application is already running");
                return Start::Unknown;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Removes the record of the copy this launcher started, once it has gone.
pub(crate) fn forget(paths: &InstallPaths) {
    let _ = std::fs::remove_file(paths.instance_record_file());
}

/// Records the copy just started, for a later start to hand over to.
///
/// Called only by the launcher holding the lock. Failing to write it costs a
/// later start its way to bring this copy to the front, nothing more.
pub(crate) fn record(paths: &InstallPaths, pid: u32) {
    let record = Record { pid, bundle: crate::reopen::opened_through(paths) };
    if let Err(error) = xpack_core::atomic::write_json(&paths.instance_record_file(), &record) {
        tracing::warn!(%error, "could not record the running copy");
    }
}

/// Removes requests named before `since`: left for a copy that has since
/// closed, and not to be replayed to the next one.
///
/// Only those, because a start that arrives while this one is still starting
/// has already been handed over, and its request is for the copy about to run.
/// Requests are named by the time they were made, which is what makes the
/// line exact.
fn clear_inbox_before(paths: &InstallPaths, since: SystemTime) {
    let Ok(entries) = std::fs::read_dir(paths.instance_inbox_dir()) else {
        return;
    };
    let line = nanos_since_epoch(since);
    for entry in entries.filter_map(std::result::Result::ok) {
        let name = entry.file_name();
        let made = name
            .to_str()
            .and_then(|name| name.split('-').next())
            .and_then(|nanos| nanos.parse::<u128>().ok());
        // A name this launcher did not write is not a request; left alone.
        if made.is_some_and(|made| made < line) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn nanos_since_epoch(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_nanos())
}

/// Hands this start over to the running copy.
///
/// Leaves the arguments in the inbox, brings the copy to the front where this
/// platform can, and says so on a terminal. Nothing here fails the start: the
/// user asked for their application and it is running.
pub(crate) fn hand_over(paths: &InstallPaths, manifest: &Manifest, arguments: &[String]) {
    if let Err(error) = leave_request(&paths.instance_inbox_dir(), arguments) {
        tracing::warn!(%error, "could not pass the arguments to the running copy");
    }

    let raised = match read_record(paths) {
        Some(record) => bring_to_front(paths, &record),
        None => false,
    };
    tracing::info!(raised, "the application is already running; handed over to it");
    xpack_core::errln!(
        "{} is already running; this start was passed to it.",
        manifest.application.name
    );
}

fn read_record(paths: &InstallPaths) -> Option<Record> {
    xpack_core::atomic::read_json(&paths.instance_record_file()).ok()
}

/// Writes one request, complete or not at all.
///
/// Named by time and process, so two starts at once cannot overwrite each
/// other, and written under another name first, so the application never
/// reads half of one.
fn leave_request(inbox: &Path, arguments: &[String]) -> xpack_core::Result<()> {
    xpack_core::atomic::create_dir_all(inbox)?;
    let nanos = nanos_since_epoch(SystemTime::now());
    let name = format!("{nanos:032}-{}.json", std::process::id());
    xpack_core::atomic::write_json(&inbox.join(name), &Request { arguments })
}

#[cfg(target_os = "macos")]
fn bring_to_front(paths: &InstallPaths, record: &Record) -> bool {
    // Checked again, not believed: the record could name a bundle that has
    // since been removed or now starts something else.
    let Some(bundle) = record
        .bundle
        .as_deref()
        .and_then(|b| crate::reopen::own_bundle(paths, Some(b.as_os_str())))
    else {
        return false;
    };
    std::process::Command::new("/usr/bin/open")
        .arg(&bundle)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(target_os = "macos"))]
fn bring_to_front(paths: &InstallPaths, record: &Record) -> bool {
    let _ = paths;
    xpack_platform::bring_to_front(record.pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_one_complete_json_file_with_the_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        let arguments = vec!["--open".to_string(), "a file \"quoted\"\n.txt".to_string()];

        leave_request(&inbox, &arguments).unwrap();
        leave_request(&inbox, &[]).unwrap();

        let mut files: Vec<_> =
            std::fs::read_dir(&inbox).unwrap().map(|entry| entry.unwrap().path()).collect();
        files.sort();
        assert_eq!(files.len(), 2, "{files:?}");
        assert!(files.iter().all(|file| file.extension().is_some_and(|e| e == "json")));

        let first: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap();
        assert_eq!(first, serde_json::json!({ "arguments": arguments }));
    }

    #[test]
    fn only_requests_from_before_the_line_are_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let paths = InstallPaths::new(dir.path(), "com.example.app").unwrap();
        leave_request(&paths.instance_inbox_dir(), &["old".into()]).unwrap();
        let line = SystemTime::now();
        std::thread::sleep(Duration::from_millis(2));
        leave_request(&paths.instance_inbox_dir(), &["new".into()]).unwrap();
        std::fs::write(paths.instance_inbox_dir().join("notes.txt"), "not a request").unwrap();

        clear_inbox_before(&paths, line);

        let mut left: Vec<String> = std::fs::read_dir(paths.instance_inbox_dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left.len(), 2, "{left:?}");
        assert_eq!(left[1], "notes.txt", "a file that is not a request was removed");
        let kept = std::fs::read(paths.instance_inbox_dir().join(&left[0])).unwrap();
        let kept: serde_json::Value = serde_json::from_slice(&kept).unwrap();
        assert_eq!(kept, serde_json::json!({ "arguments": ["new"] }));
    }

    #[test]
    fn a_second_start_finds_the_first_one() {
        let dir = tempfile::tempdir().unwrap();
        let paths = InstallPaths::new(dir.path(), "com.example.app").unwrap();

        let first = begin(&paths, false);
        assert!(matches!(first, Start::First(_)), "{first:?}");
        assert!(matches!(begin(&paths, false), Start::Another));
        drop(first);
        assert!(matches!(begin(&paths, false), Start::First(_)));
    }
}
