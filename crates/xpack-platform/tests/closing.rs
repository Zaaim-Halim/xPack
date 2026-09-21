//! Asking a process to close.
//!
//! What "asking" means differs by platform, and so does what can be proven
//! about it here.
//!
//! On Unix the request is `SIGTERM`, which reaches any process at all, so a
//! test can ask one to close and watch it go.
//!
//! On Windows the request is `WM_CLOSE`, posted to the process's windows — the
//! same message the close button sends. A process with no window has nothing
//! to receive it, and `taskkill` says so rather than terminating it anyway.
//! That refusal is the guarantee worth pinning: `/F` would end the process
//! regardless, and it is never passed, because a user's unsaved work is
//! exactly what it would take with it.
//!
//! Neither arm can test the case that matters most in the field: an
//! application that *refuses*, putting up "you have unsaved changes" and
//! staying open. Nothing that can be spawned from a test has anything to
//! refuse with. That behaviour belongs to the applications xPack launches, and
//! the contract here is only that they are asked rather than killed.

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

/// A process that stays alive until something stops it, with no window.
fn a_running_process() -> std::process::Child {
    let mut command = if cfg!(windows) {
        // `ping` waits a second between echoes, so this lives about a minute.
        // Chosen because every Windows install has it; `sleep` is not a
        // Windows command at all and arrives only with other software.
        let mut command = Command::new("ping");
        command.args(["-n", "60", "127.0.0.1"]);
        command
    } else {
        let mut command = Command::new("sleep");
        command.arg("60");
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("a process to ask")
}

#[cfg(unix)]
#[test]
fn a_running_process_is_asked_to_close_and_does() {
    let mut child = a_running_process();

    request_close(child.id()).expect("the request to be delivered");

    assert!(
        wait_for_exit(&mut child, Duration::from_secs(5)),
        "the process was still running five seconds after being asked to close"
    );
    let _ = child.wait();
}

#[cfg(windows)]
#[test]
fn a_process_with_no_window_is_left_running_rather_than_forced() {
    // The guarantee: where the request cannot be delivered, the answer is a
    // reported failure and a process that is still there. Passing `/F` would
    // make this test pass by killing the thing it is asking politely.
    let mut child = a_running_process();

    let err = request_close(child.id()).expect_err("a windowless process cannot be asked");
    assert!(err.to_string().contains("close request"), "got {err}");

    assert!(
        !wait_for_exit(&mut child, Duration::from_millis(500)),
        "the process was ended despite the request being refused"
    );

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn asking_a_process_that_has_already_gone_is_reported_rather_than_hidden() {
    // The launcher asks once and then waits; if the application had already
    // exited by itself, the caller needs to be able to tell that apart from a
    // request that worked.
    let mut child = if cfg!(windows) {
        Command::new("cmd").args(["/c", "exit"]).spawn().expect("a process")
    } else {
        Command::new("true").spawn().expect("a process")
    };
    let pid = child.id();
    let _ = child.wait();

    // The pid may be recycled in principle; in practice not this fast, and a
    // test that tolerated either answer would be testing nothing.
    assert!(request_close(pid).is_err());
}
