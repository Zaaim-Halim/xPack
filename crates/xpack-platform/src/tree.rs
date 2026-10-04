//! Starting a program so that it can be stopped together with everything it
//! started.
//!
//! A package's hook runs programs, and those may start others. When the hook
//! runs out of time, or the person installing cancels, stopping only the
//! program xPack started would leave the rest running on: a half-done service
//! registration, an installer of something else, a shell loop.
//!
//! # What is reached
//!
//! On Unix the program is started as the leader of a process group of its
//! own, which everything it starts joins, and the whole group is killed. A
//! process that deliberately leaves the group (`setsid`, the usual way a
//! daemon detaches) is not reached: it has asked not to be.
//!
//! On Windows the tree is walked by `taskkill /T` from the program down,
//! through each process's parent. A process whose parent has already exited
//! is no longer linked to the tree and is not reached. While the program
//! itself is still running, as it is when its time runs out, its own
//! descendants are linked to it.
//!
//! A job object would reach those too, at the price of unsafe Win32 calls;
//! this crate keeps to its one, and takes `taskkill`, as [`request_close`]
//! does.
//!
//! Neither platform stops anything when the program ends on its own: what it
//! left running is what it meant to leave running.
//!
//! [`request_close`]: crate::request_close

use std::io;
use std::process::{Child, Command};

/// Makes `command` start its program as the head of a tree [`stop`] can end.
pub fn start_as_tree(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = command;
}

/// Ends `child`, started with [`start_as_tree`], and everything it started,
/// and waits for `child` to be gone.
pub fn stop(child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    stop_descendants(child);
    // In case the tree could not be reached, the program itself at least.
    let _ = child.kill();
    child.wait().map(drop)
}

/// Ends this process and everything it started, when it was started with
/// [`start_as_tree`]: for a program whose own starter has gone, so that
/// nothing else would stop what it runs. Returns only if this process could
/// not be ended so; the caller then exits.
pub fn stop_own_tree() {
    #[cfg(unix)]
    {
        let me = rustix::process::getpid();
        // Only a group this process leads, which is what `start_as_tree`
        // made: never the group of whatever started it otherwise.
        if rustix::process::getpgrp() == me {
            let _ = rustix::process::kill_process_group(me, rustix::process::Signal::KILL);
        }
    }
    #[cfg(not(unix))]
    {
        use std::process::Stdio;
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let mut command =
            Command::new(std::path::Path::new(&system).join("System32\\taskkill.exe"));
        command
            .args(["/T", "/F", "/PID"])
            .arg(std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crate::without_a_console(&mut command);
        let _ = command.status();
    }
}

#[cfg(unix)]
fn stop_descendants(child: &Child) {
    let group = rustix::process::Pid::from_child(child);
    if let Err(error) = rustix::process::kill_process_group(group, rustix::process::Signal::KILL) {
        tracing::warn!(%error, "could not stop the programs a hook started");
    }
}

#[cfg(not(unix))]
fn stop_descendants(child: &Child) {
    use std::process::Stdio;
    // From the system directory, never found along `PATH`.
    let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let mut command = Command::new(std::path::Path::new(&system).join("System32\\taskkill.exe"));
    command
        .args(["/T", "/F", "/PID"])
        .arg(child.id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::without_a_console(&mut command);
    if let Err(error) = command.status() {
        tracing::warn!(%error, "could not stop the programs a hook started");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    fn alive(pid: i32) -> bool {
        rustix::process::Pid::from_raw(pid)
            .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
    }

    #[test]
    fn stopping_a_program_stops_what_it_started_too() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 60 & echo $!; sleep 60"]).stdout(Stdio::piped());
        start_as_tree(&mut command);
        let mut child = command.spawn().unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let grandchild: i32 = line.trim().parse().unwrap();
        assert!(alive(grandchild));

        stop(&mut child).unwrap();

        // Reparented and reaped by the system once killed; give it a moment.
        let started = Instant::now();
        while alive(grandchild) && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(grandchild), "what the program started is still running");
    }

    #[test]
    fn stopping_a_program_that_already_ended_is_not_an_error() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        start_as_tree(&mut command);
        let mut child = command.spawn().unwrap();
        child.wait().unwrap();
        stop(&mut child).unwrap();
    }
}
