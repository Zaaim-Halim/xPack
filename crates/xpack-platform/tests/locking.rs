//! Cross-process locking, exercised with an actual second process.
//!
//! Taking the lock twice inside one process does block, but that is not the
//! code path a real concurrent xPack takes. These tests re-execute the test
//! binary so the contention is genuinely between two operating-system
//! processes.

use std::path::Path;
use std::process::Command;

use xpack_core::{InstallPaths, InstallState, Version};
use xpack_platform::InstallLock;

/// Set on the child to select its role. Read only; never set in-process,
/// because `std::env::set_var` is unsafe in this edition.
const CHILD_ROLE: &str = "XPACK_TEST_CHILD_ROLE";
const CHILD_DIR: &str = "XPACK_TEST_CHILD_DIR";

/// Exit codes the child uses to report what it observed.
const CHILD_SAW_LOCKED: i32 = 10;
const CHILD_ACQUIRED: i32 = 11;
const CHILD_OTHER_ERROR: i32 = 12;

fn paths_in(root: &Path) -> InstallPaths {
    InstallPaths::new(root, "com.example.app").unwrap()
}

/// Runs this test binary again, with the child role selected.
fn spawn_child(role: &str, root: &Path, test_name: &str) -> std::process::ExitStatus {
    let exe = std::env::current_exe().expect("test binary path");
    Command::new(exe)
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(CHILD_ROLE, role)
        .env(CHILD_DIR, root)
        .status()
        .expect("child process should start")
}

/// Performs the child half: try to take the lock, report what happened.
fn run_child_if_selected(expected_role: &str) -> bool {
    let Ok(role) = std::env::var(CHILD_ROLE) else {
        return false;
    };
    if role != expected_role {
        return false;
    }
    let root = std::env::var(CHILD_DIR).expect("child directory");
    let paths = paths_in(Path::new(&root));

    let code = match InstallLock::acquire(&paths) {
        Err(xpack_core::Error::Locked(_)) => CHILD_SAW_LOCKED,
        Ok(_guard) => CHILD_ACQUIRED,
        Err(_) => CHILD_OTHER_ERROR,
    };
    std::process::exit(code);
}

#[test]
fn a_second_process_cannot_take_a_held_lock() {
    if run_child_if_selected("contend") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());

    let held = InstallLock::acquire(&paths).expect("first acquisition must succeed");

    let status = spawn_child("contend", dir.path(), "a_second_process_cannot_take_a_held_lock");
    assert_eq!(
        status.code(),
        Some(CHILD_SAW_LOCKED),
        "a separate process must be refused while the lock is held"
    );
    drop(held);
}

#[test]
fn the_lock_is_released_when_the_holder_drops_it() {
    if run_child_if_selected("after_release") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());

    // Take and release before the child runs.
    drop(InstallLock::acquire(&paths).unwrap());

    let status =
        spawn_child("after_release", dir.path(), "the_lock_is_released_when_the_holder_drops_it");
    assert_eq!(
        status.code(),
        Some(CHILD_ACQUIRED),
        "a released lock must be available to the next process"
    );
}

#[test]
fn the_lock_file_survives_release_so_the_inode_is_stable() {
    // On Unix the lock belongs to the inode, not the path. If releasing
    // removed the file, the next process would create and lock a *different*
    // inode at the same path, and mutual exclusion would silently vanish.
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());

    let lock = InstallLock::acquire(&paths).unwrap();
    let before = std::fs::metadata(paths.lock_file()).expect("lock file exists while held");
    drop(lock);

    let after = std::fs::metadata(paths.lock_file()).expect("lock file must outlive the lock");

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(before.ino(), after.ino(), "the lock inode must be stable");
    }
    #[cfg(not(unix))]
    {
        let _ = (before, after);
    }
}

#[test]
fn acquiring_does_not_truncate_the_lock_file() {
    // `File::create` truncates, which writes to a file another process may be
    // mid-operation with. Acquisition must not disturb existing content.
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    xpack_core::atomic::create_dir_all(&paths.state_dir()).unwrap();
    std::fs::write(paths.lock_file(), b"owner metadata").unwrap();

    let lock = InstallLock::acquire(&paths).unwrap();
    assert_eq!(std::fs::read(paths.lock_file()).unwrap(), b"owner metadata");
    drop(lock);
}

#[test]
fn state_is_reachable_only_through_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    // Nothing installed yet.
    let mut state = lock.load_or_new_state("com.example.app").unwrap();
    assert!(state.current_version.is_none());

    state.current_version = Some(Version::parse("1.0.0").unwrap());
    state.stage_version(&Version::parse("1.0.0").unwrap(), None);
    lock.save_state(&state).unwrap();

    let reloaded = lock.load_state().unwrap();
    assert_eq!(reloaded.value.current_version, state.current_version);
    assert!(!reloaded.recovered_from_backup);
}

#[test]
fn a_lost_primary_recovers_from_the_backup_rather_than_starting_over() {
    // Testing only the primary would discard a perfectly good backup. The
    // installation would look brand new, orphaning every installed version and
    // losing which ones were healthy — the exact failure the backup prevents.
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let mut state = InstallState::new("com.example.app");
    let one = Version::parse("1.0.0").unwrap();
    state.current_version = Some(one.clone());
    state.stage_version(&one, None);
    state.mark_good(&one);
    lock.save_state(&state).unwrap();
    // A second save rotates the first into the backup.
    lock.save_state(&state).unwrap();

    std::fs::remove_file(paths.state_file()).unwrap();
    assert!(xpack_core::store::backup_path(&paths.state_file()).exists());

    let recovered = lock.load_or_new_state("com.example.app").unwrap();
    assert_eq!(recovered.current_version, Some(one.clone()), "the backup must be used");
    assert!(recovered.is_good(&one), "version health records must survive");
}

#[test]
fn state_for_a_different_application_is_refused() {
    // The layout and the state document each carry an application id, and
    // nothing else forces them to agree. Writing one application's records
    // into another's directory would corrupt both installations.
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let foreign = InstallState::new("com.someone.else");
    let err = lock.save_state(&foreign).unwrap_err();
    assert!(err.to_string().contains("com.someone.else"), "got {err}");
    assert!(!paths.state_file().exists(), "nothing may be written");
}

#[test]
fn state_from_a_different_application_is_refused_on_read() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    // Write a foreign document past the guard, as a stray file would appear.
    InstallState::new("com.someone.else").save(&paths.state_file()).unwrap();
    assert!(lock.load_state().is_err(), "a foreign document must not be trusted");
}

#[test]
fn a_genuinely_empty_installation_still_starts_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let state = lock.load_or_new_state("com.example.app").unwrap();
    assert!(state.current_version.is_none());
    assert!(state.versions.is_empty());
}

#[test]
fn a_corrupt_state_file_is_not_mistaken_for_a_fresh_install() {
    // Silently starting over would orphan every installed version and lose the
    // record of which ones were healthy.
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    lock.save_state(&InstallState::new("com.example.app")).unwrap();
    std::fs::write(paths.state_file(), b"corrupt").unwrap();
    std::fs::write(xpack_core::store::backup_path(&paths.state_file()), b"corrupt too").unwrap();

    assert!(lock.load_or_new_state("com.example.app").is_err());
}

/// Exit codes the download-lease children use.
const CHILD_LEASE_FREE: i32 = 20;
const CHILD_LEASE_HELD: i32 = 21;

/// Performs the child half of a lease probe: report whether anyone holds it.
fn run_lease_child_if_selected(expected_role: &str) -> bool {
    let Ok(role) = std::env::var(CHILD_ROLE) else {
        return false;
    };
    if role != expected_role {
        return false;
    }
    let root = std::env::var(CHILD_DIR).expect("child directory");
    let paths = paths_in(Path::new(&root));

    let code = match xpack_platform::DownloadLease::is_held(&paths) {
        Ok(true) => CHILD_LEASE_HELD,
        Ok(false) => CHILD_LEASE_FREE,
        Err(_) => CHILD_OTHER_ERROR,
    };
    std::process::exit(code);
}

#[test]
fn a_download_lease_is_visible_to_another_process() {
    // This is what lets recovery tell a download in progress from debris left
    // by a process that died.
    if run_lease_child_if_selected("lease-held") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());

    let lease = xpack_platform::DownloadLease::acquire(&paths).unwrap();
    assert!(lease.is_some(), "the lease should have been free");

    let status =
        spawn_child("lease-held", dir.path(), "a_download_lease_is_visible_to_another_process");
    assert_eq!(status.code(), Some(CHILD_LEASE_HELD), "another process could not see the lease");
}

#[test]
fn a_download_lease_is_released_when_its_holder_ends() {
    // The whole reason the lease is a file lock and not a timestamp: a process
    // that dies, however it dies, stops holding it immediately. A timestamp
    // lease would still be live and owned by nobody.
    if run_lease_child_if_selected("lease-free") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());

    {
        let lease = xpack_platform::DownloadLease::acquire(&paths).unwrap();
        assert!(lease.is_some());
    }

    let status =
        spawn_child("lease-free", dir.path(), "a_download_lease_is_released_when_its_holder_ends");
    assert_eq!(status.code(), Some(CHILD_LEASE_FREE), "the lease outlived its holder");
}
