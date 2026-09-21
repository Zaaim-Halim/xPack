//! Asking a process to close.
//!
//! The claim being tested is narrow and worth stating: the request reaches the
//! process and the process goes away. Whether an application *refuses* — puts
//! up "you have unsaved changes" and stays running — cannot be tested with a
//! shell script, because a shell script has nothing to refuse with. That
//! behaviour belongs to the applications xPack launches, and the contract here
//! is only that they are asked rather than killed.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use xpack_platform::request_close;

/// Waits for a child to exit, up to a limit, so a hang fails as a test rather
/// than as a test run that never ends.
fn wait_for_exit(child: &mut std::process::Child, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn a_running_process_is_asked_to_close_and_does() {
    let mut child = Command::new("sleep")
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("a process to ask");

    request_close(child.id()).expect("the request to be delivered");

    assert!(
        wait_for_exit(&mut child, Duration::from_secs(5)),
        "the process was still running five seconds after being asked to close"
    );
    let _ = child.kill();
}

#[test]
fn asking_a_process_that_has_already_gone_is_reported_rather_than_hidden() {
    // The launcher asks once and then waits; if the application had already
    // exited by itself, the caller needs to be able to tell that apart from a
    // request that worked.
    let mut child = Command::new("true").spawn().expect("a process");
    let pid = child.id();
    let _ = child.wait();

    // The pid may be recycled in principle; in practice not this fast, and a
    // test that tolerated either answer would be testing nothing.
    assert!(request_close(pid).is_err());
}
