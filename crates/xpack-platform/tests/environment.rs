//! Announcing an environment change.
//!
//! What the broadcast achieves, a new terminal that sees the new `PATH`, can
//! only be seen by a person on a Windows desktop. What a test can hold it to
//! is that the call is safe to make from an installer: it returns, promptly,
//! whatever the machine's windows do with it.

use std::time::{Duration, Instant};

use xpack_platform::announce_environment_change;

#[cfg(windows)]
#[test]
fn announcing_returns_promptly_even_with_nobody_answering() {
    // A build machine has no one signed in, or windows that never answer.
    // Each window gets a second; the whole call must still end.
    let started = Instant::now();
    let _delivered = announce_environment_change();
    let _again = announce_environment_change();
    assert!(started.elapsed() < Duration::from_secs(60), "took {:?}", started.elapsed());
}

#[cfg(not(windows))]
#[test]
fn elsewhere_there_is_nothing_to_announce() {
    let started = Instant::now();
    assert!(!announce_environment_change());
    assert!(started.elapsed() < Duration::from_secs(1));
}
