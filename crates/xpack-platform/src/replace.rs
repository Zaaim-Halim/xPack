//! Replacing a program on disk.
//!
//! Two questions an installer asks before and while it replaces xPack's own
//! programs in an installation: is this program running, and can this file be
//! moved now. Both have answers that differ by platform.
//!
//! On macOS and Linux a running program's file can be renamed over at any
//! time; the running process keeps the old file until it ends. On Windows a
//! running program's file cannot be written, deleted or replaced, only
//! renamed, and any other program holding a file open (an antivirus scanner,
//! the search indexer) can briefly refuse even that.

use std::path::Path;
use std::time::{Duration, Instant};

use xpack_core::{Error, Result};

/// How long a rename keeps trying while something briefly holds the file.
///
/// Scanners and indexers open a newly written program for well under a
/// second; two is enough to wait them out, and short enough that a file held
/// for good is reported rather than waited on.
pub const RENAME_PATIENCE: Duration = Duration::from_secs(2);

/// Whether the program at `path` is running, as far as its file says.
///
/// **Windows**: the file is opened for writing, which changes nothing in it,
/// and Windows refuses that for the file of a running program, whoever
/// started it. A file that does not exist is not in use.
///
/// **macOS and Linux**: always `false`. A running program's file gives no
/// sign of it there, and needs none: it can be replaced while it runs.
/// Whether the installation is running is asked of its locks instead.
///
/// # Errors
///
/// When the file exists and cannot be opened for a reason other than being
/// in use: a permission refused, say. That is not a running program, and
/// guessing either way would be wrong.
pub fn program_in_use(path: &Path) -> Result<bool> {
    program_in_use_on_this_platform(path)
}

#[cfg(windows)]
fn program_in_use_on_this_platform(path: &Path) -> Result<bool> {
    use std::os::windows::fs::OpenOptionsExt;

    // Others may go on reading and starting it while it is asked: only the
    // write access is the question, and a launcher starting this instant
    // must not be refused because of it.
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_DELETE: u32 = 0x4;

    match std::fs::OpenOptions::new()
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .open(path)
    {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) if is_sharing_refusal(&error) => Ok(true),
        Err(error) => Err(Error::io(path, error)),
    }
}

// The same signature as the Windows version, which can fail.
#[cfg(not(windows))]
#[allow(clippy::unnecessary_wraps)]
fn program_in_use_on_this_platform(path: &Path) -> Result<bool> {
    let _ = path;
    Ok(false)
}

/// Renames `from` to `to`, replacing `to` if it exists, and keeps trying for
/// up to [`RENAME_PATIENCE`] while another program briefly holds either file.
///
/// Anything other than a file held by another program fails at once.
///
/// # Errors
///
/// When the rename fails for another reason, or the file is still held when
/// the patience runs out.
pub fn rename_when_free(from: &Path, to: &Path) -> Result<()> {
    rename_within(from, to, RENAME_PATIENCE)
}

fn rename_within(from: &Path, to: &Path, patience: Duration) -> Result<()> {
    let deadline = Instant::now() + patience;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if is_held_by_another(&error) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                return Err(Error::io(from, error));
            }
        }
    }
}

/// Whether `error` is Windows refusing to share a file another program has
/// open: what opening a running program's file for writing gives.
///
/// Narrower than [`is_held_by_another`] on purpose. "Access denied" is also
/// what a read-only file, or one this account may not write, gives; counted
/// as in use, it would have an installer report a closed application as open
/// for as long as anyone waited.
#[cfg(windows)]
fn is_sharing_refusal(error: &std::io::Error) -> bool {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    matches!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION))
}

/// Whether `error` says another program holds the file, for now.
///
/// Windows reports a scanner or indexer holding a file as a sharing or lock
/// violation, and sometimes as access denied, which is also what renaming
/// over a running program's file gives. Elsewhere nothing a rename meets is
/// passing, so nothing is waited on.
fn is_held_by_another(error: &std::io::Error) -> bool {
    #[cfg(windows)]
    {
        const ERROR_ACCESS_DENIED: i32 = 5;
        const ERROR_SHARING_VIOLATION: i32 = 32;
        const ERROR_LOCK_VIOLATION: i32 = 33;
        matches!(
            error.raw_os_error(),
            Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
        )
    }
    #[cfg(not(windows))]
    {
        let _ = error;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rename_replaces_what_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("new");
        let to = dir.path().join("program");
        std::fs::write(&from, "new").unwrap();
        std::fs::write(&to, "old").unwrap();

        rename_when_free(&from, &to).unwrap();

        assert_eq!(std::fs::read_to_string(&to).unwrap(), "new");
        assert!(!from.exists());
    }

    #[test]
    fn a_rename_that_cannot_happen_fails_at_once_and_names_the_file() {
        // Nothing passing about a missing file: it is not waited on.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let started = Instant::now();

        let error = rename_within(&missing, &dir.path().join("to"), Duration::from_secs(30))
            .unwrap_err()
            .to_string();

        assert!(started.elapsed() < Duration::from_secs(5), "it waited for a missing file");
        assert!(error.contains("missing"), "{error}");
    }

    #[test]
    fn a_file_that_is_not_there_is_not_in_use() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!program_in_use(&dir.path().join("nothing")).unwrap());
    }

    #[cfg(not(windows))]
    #[test]
    fn a_running_program_does_not_show_in_its_file_outside_windows() {
        // Running, and still reported free: on these systems the locks say
        // whether an installation runs, and the file can be replaced anyway.
        let program = std::env::current_exe().unwrap();
        assert!(!program_in_use(&program).unwrap());
    }

    /// A copy of Windows' own `ping`, started so it runs for a few seconds:
    /// a program that is certainly running while the test asks about it.
    #[cfg(windows)]
    fn running_copy(dir: &Path) -> (std::path::PathBuf, std::process::Child) {
        let system = std::env::var_os("SystemRoot").unwrap();
        let ping = Path::new(&system).join("System32").join("PING.EXE");
        let copy = dir.join("busy.exe");
        std::fs::copy(ping, &copy).unwrap();
        let child = std::process::Command::new(&copy)
            .args(["-n", "6", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // Long enough for it to be loaded, far short of its five seconds.
        std::thread::sleep(Duration::from_millis(500));
        (copy, child)
    }

    #[cfg(windows)]
    #[test]
    fn a_running_program_is_in_use_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let (copy, mut child) = running_copy(dir.path());

        let in_use = program_in_use(&copy);
        let _ = child.kill();
        let _ = child.wait();

        assert!(in_use.unwrap(), "a running program was reported free");
        // Windows lets go of the file a moment after the process ends.
        let deadline = Instant::now() + Duration::from_secs(5);
        while program_in_use(&copy).unwrap() {
            assert!(Instant::now() < deadline, "a stopped program was still reported in use");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_file_nobody_runs_is_not_in_use_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("idle.exe");
        std::fs::write(&file, "not running").unwrap();
        assert!(!program_in_use(&file).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn a_read_only_file_is_an_error_not_a_running_program_on_windows() {
        // Windows answers "access denied" here, as it can for a running
        // program; taking it for one would keep an installer waiting for an
        // application that is closed.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("read-only.exe");
        std::fs::write(&file, "not running").unwrap();
        let mut permissions = std::fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&file, permissions.clone()).unwrap();

        let answer = program_in_use(&file);
        // Windows has no "world-writable" for this to open up, which is what
        // the lint guards against; clearing the flag only lets the temporary
        // directory be removed.
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&file, permissions).unwrap();

        assert!(answer.is_err(), "a read-only file was taken for a running program: {answer:?}");
    }

    #[cfg(windows)]
    #[test]
    fn a_running_program_can_be_renamed_aside_but_not_replaced_on_windows() {
        // The two facts the replacement of an installation for everyone
        // stands on: the running file moves aside, and nothing can be renamed
        // over it while it runs.
        let dir = tempfile::tempdir().unwrap();
        let (copy, mut child) = running_copy(dir.path());
        let incoming = dir.path().join("incoming.exe");
        std::fs::write(&incoming, "new").unwrap();

        let over = rename_within(&incoming, &copy, Duration::from_millis(200));
        let aside = dir.path().join("busy.exe.xpack-old-1");
        let moved = rename_when_free(&copy, &aside);
        let _ = child.kill();
        let _ = child.wait();

        assert!(over.is_err(), "a running program was replaced in place");
        moved.unwrap();
        assert!(aside.exists() && !copy.exists());
    }
}
