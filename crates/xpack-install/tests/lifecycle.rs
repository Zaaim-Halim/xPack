//! Install, activate, roll back and recover, against real signed packages.

mod common;

use common::{build_package, install_paths, installed_names};
use xpack_core::state::{UpdatePhase, VersionStatus};
use xpack_core::{InstallState, Version};
use xpack_install::{InstallOptions, Installer, TrustDecision, open_and_verify};
use xpack_platform::InstallLock;
use xpack_security::KeyPair;

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

/// Installs `version` into a locked installation.
fn install(
    lock: &InstallLock,
    dir: &std::path::Path,
    key: &KeyPair,
    version: &str,
    options: &InstallOptions,
) -> xpack_core::Result<xpack_install::Installed> {
    let package = build_package(dir, key, version);
    let mut verified = open_and_verify(&package, lock, &TrustDecision::Explicit(key.public()))?;
    Installer::new(lock).install(&mut verified, options)
}

#[test]
fn a_first_install_becomes_active_and_healthy() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let options = InstallOptions { activate: true, ..Default::default() };
    let installed = install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();
    assert!(installed.activated);

    let state = lock.load_state().unwrap().value;
    assert_eq!(state.current_version, Some(v("1.0.0")));
    // A first install has nothing to roll back to, so there is no probation.
    assert!(state.update.is_idle(), "got {:?}", state.update);
    assert!(state.is_good(&v("1.0.0")));

    assert!(paths.version_dir(&v("1.0.0")).join("bin/app").is_file());
}

#[test]
fn each_installed_version_keeps_the_manifest_it_came_from() {
    // Without this a version can never serve as a differential update base.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    install(&lock, dir.path(), &key, "1.0.0", &InstallOptions::default()).unwrap();

    let manifest_file = paths.version_manifest_file(&v("1.0.0"));
    let signature_file = paths.version_signature_file(&v("1.0.0"));
    assert!(manifest_file.is_file(), "the manifest must be recorded");
    assert!(signature_file.is_file(), "the signature must be recorded");

    // The bytes must be verbatim, or the signature beside them will not verify.
    let bytes = std::fs::read(&manifest_file).unwrap();
    let signature =
        xpack_security::Signature::parse_hex(&std::fs::read_to_string(&signature_file).unwrap())
            .unwrap();
    xpack_security::verify(&key.public(), &bytes, &signature)
        .expect("the recorded signature must verify against the recorded bytes");
}

#[test]
fn installing_a_second_version_puts_it_on_probation() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let options = InstallOptions { activate: true, ..Default::default() };

    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();
    install(&lock, dir.path(), &key, "1.1.0", &options).unwrap();

    let state = lock.load_state().unwrap().value;
    assert_eq!(state.current_version, Some(v("1.1.0")));
    match state.update {
        UpdatePhase::PendingVerification { version, rollback_to, attempts } => {
            assert_eq!(version, v("1.1.0"));
            assert_eq!(rollback_to, v("1.0.0"), "the previous version is the rollback target");
            assert_eq!(attempts, 0);
        }
        other => panic!("expected probation, got {other:?}"),
    }
}

#[test]
fn a_failed_version_rolls_back_to_the_last_healthy_one() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let installer = Installer::new(&lock);
    let options = InstallOptions { activate: true, ..Default::default() };

    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();
    install(&lock, dir.path(), &key, "1.1.0", &options).unwrap();

    let restored = installer.record_failure("crashed on startup").unwrap();
    assert_eq!(restored, Some(v("1.0.0")));

    let state = lock.load_state().unwrap().value;
    assert_eq!(state.current_version, Some(v("1.0.0")));
    assert!(state.is_bad(&v("1.1.0")), "the failing version must be quarantined");
    assert!(state.update.is_idle());
}

#[test]
fn a_quarantined_version_is_never_activated_again() {
    // This is what stops an update loop.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let installer = Installer::new(&lock);
    let options = InstallOptions { activate: true, ..Default::default() };

    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();
    install(&lock, dir.path(), &key, "1.1.0", &options).unwrap();
    installer.record_failure("crashed").unwrap();

    let err = installer.activate(&v("1.1.0"), true).unwrap_err();
    assert!(err.to_string().contains("health check"), "got {err}");
}

#[test]
fn probation_ends_after_a_bounded_number_of_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let installer = Installer::new(&lock);
    let options = InstallOptions { activate: true, ..Default::default() };

    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();
    install(&lock, dir.path(), &key, "1.1.0", &options).unwrap();

    let mut phase = installer.begin_attempt().unwrap();
    assert!(!phase.attempts_exhausted(), "one attempt is not exhaustion");
    phase = installer.begin_attempt().unwrap();
    assert!(phase.attempts_exhausted(), "probation must terminate");
}

#[test]
fn committing_health_ends_probation() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let installer = Installer::new(&lock);
    let options = InstallOptions { activate: true, ..Default::default() };

    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();
    install(&lock, dir.path(), &key, "1.1.0", &options).unwrap();
    installer.commit_health().unwrap();

    let state = lock.load_state().unwrap().value;
    assert!(state.update.is_idle());
    assert!(state.is_good(&v("1.1.0")));
}

#[test]
fn a_package_for_another_application_is_refused() {
    // The same seam that has produced bugs elsewhere: two components each
    // carrying an application id with nothing forcing them to agree.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let foreign = xpack_core::InstallPaths::new(dir.path(), "com.other.app").unwrap();
    let lock = InstallLock::acquire(&foreign).unwrap();

    let package = build_package(dir.path(), &key, "1.0.0");
    let mut verified =
        open_and_verify(&package, &lock, &TrustDecision::Explicit(key.public())).unwrap();
    let err = Installer::new(&lock).install(&mut verified, &InstallOptions::default()).unwrap_err();
    assert!(err.to_string().contains("com.example.app"), "got {err}");
}

#[test]
fn a_downgrade_is_refused_unless_requested() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let options = InstallOptions { activate: true, ..Default::default() };

    install(&lock, dir.path(), &key, "1.1.0", &options).unwrap();
    let err = install(&lock, dir.path(), &key, "1.0.0", &options).unwrap_err();
    assert!(matches!(err, xpack_core::Error::DowngradeRejected { .. }), "got {err:?}");

    let allowed = InstallOptions { activate: true, allow_downgrade: true, ..Default::default() };
    install(&lock, dir.path(), &key, "1.0.0", &allowed).expect("explicit override must work");
}

#[test]
fn activation_re_checks_for_a_downgrade() {
    // The check before download is not enough on its own: state can change
    // while a long operation runs, so activation must check again against
    // whatever is current by then.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let installer = Installer::new(&lock);

    // Both installed, neither activated yet, so neither install saw a
    // downgrade at install time.
    install(&lock, dir.path(), &key, "1.0.0", &InstallOptions::default()).unwrap();
    install(&lock, dir.path(), &key, "1.1.0", &InstallOptions::default()).unwrap();

    installer.activate(&v("1.1.0"), false).unwrap();

    let err = installer.activate(&v("1.0.0"), false).unwrap_err();
    assert!(
        matches!(err, xpack_core::Error::DowngradeRejected { .. }),
        "activation must refuse a downgrade, got {err:?}"
    );
    installer.activate(&v("1.0.0"), true).expect("an explicit override must still work");
}

#[test]
fn installing_the_same_version_twice_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    install(&lock, dir.path(), &key, "1.0.0", &InstallOptions::default()).unwrap();
    let err = install(&lock, dir.path(), &key, "1.0.0", &InstallOptions::default()).unwrap_err();
    assert!(err.to_string().contains("already installed"), "got {err}");
}

#[test]
fn pruning_keeps_everything_recovery_might_need() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let installer = Installer::new(&lock);
    let options = InstallOptions { activate: true, ..Default::default() };

    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();
    installer.commit_health().unwrap();
    install(&lock, dir.path(), &key, "1.1.0", &options).unwrap();
    installer.commit_health().unwrap();
    install(&lock, dir.path(), &key, "1.2.0", &options).unwrap();
    installer.commit_health().unwrap();

    installer.prune().unwrap();
    let state = lock.load_state().unwrap().value;
    assert!(state.versions.contains_key("1.2.0"), "the active version must survive");
    assert!(state.versions.contains_key("1.1.0"), "the rollback target must survive");
    assert!(paths.version_dir(&v("1.2.0")).exists());
}

#[test]
fn uninstalling_removes_the_whole_installation() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let options = InstallOptions { activate: true, ..Default::default() };

    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();
    let removal = xpack_install::uninstall(lock).unwrap();

    assert!(removal.is_complete(), "left behind: {:?}", removal.remaining);
    assert!(!paths.root().exists(), "the installation root survived");
}

#[test]
fn uninstalling_removes_the_pinned_signing_keys() {
    // The consequential part of an incomplete uninstall: a surviving trust
    // store means a later reinstall inherits a trust decision the user
    // believes they revoked.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    install(&lock, dir.path(), &key, "1.0.0", &InstallOptions::default()).unwrap();
    let mut store = xpack_security::trust::TrustStore::new();
    store.trust(&key.public(), "test");
    store.save(&paths.trust_file()).unwrap();
    assert!(paths.trust_file().is_file(), "precondition: a key is pinned");

    xpack_install::uninstall(lock).unwrap();

    assert!(!paths.trust_file().exists(), "the pinned signing key survived uninstall");
}

#[test]
fn uninstalling_leaves_unexpected_content_in_the_root_alone() {
    // The root is emptied non-recursively, so anything xPack did not put there
    // is reported rather than deleted.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    install(&lock, dir.path(), &key, "1.0.0", &InstallOptions::default()).unwrap();
    let stray = paths.root().join("user-notes.txt");
    std::fs::write(&stray, b"keep me").unwrap();

    let removal = xpack_install::uninstall(lock).unwrap();

    assert!(!removal.is_complete(), "the root should have been kept");
    assert_eq!(removal.remaining, vec![stray.clone()]);
    assert_eq!(std::fs::read(&stray).unwrap(), b"keep me");
    assert!(!paths.versions_dir().exists(), "versions should still be gone");
    assert!(!paths.state_dir().exists(), "state should still be gone");
}

#[test]
fn installed_lists_versions_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let installer = Installer::new(&lock);

    install(&lock, dir.path(), &key, "1.0.0", &InstallOptions::default()).unwrap();
    install(&lock, dir.path(), &key, "1.2.0", &InstallOptions::default()).unwrap();
    install(&lock, dir.path(), &key, "1.10.0", &InstallOptions::default()).unwrap();

    let listed = installer.installed().unwrap();
    let order: Vec<String> = listed.iter().map(|(v, _)| v.to_string()).collect();
    assert_eq!(order, ["1.10.0", "1.2.0", "1.0.0"], "must sort numerically, not lexically");
    assert!(listed.iter().all(|(_, s)| *s == VersionStatus::Staged));
}

#[test]
fn trust_on_first_use_pins_the_declared_key() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let package = build_package(dir.path(), &key, "1.0.0");
    let mut verified = open_and_verify(&package, &lock, &TrustDecision::OnFirstUse).unwrap();
    Installer::new(&lock).install(&mut verified, &InstallOptions::default()).unwrap();

    let store = xpack_security::TrustStore::load_or_empty(&paths.trust_file()).unwrap();
    assert!(store.contains(&key.public()), "the declared key must be pinned");

    // A different publisher must now be refused: first use has passed.
    let attacker = KeyPair::generate().unwrap();
    let forged = build_package(dir.path(), &attacker, "2.0.0");
    assert!(open_and_verify(&forged, &lock, &TrustDecision::OnFirstUse).is_err());
}

#[test]
fn an_installation_trusting_nothing_refuses_to_install() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let package = build_package(dir.path(), &key, "1.0.0");
    let err = open_and_verify(&package, &lock, &TrustDecision::UsePinned).unwrap_err();
    assert!(err.to_string().contains("trusts no signing keys"), "got {err}");
}

// --- state and the filesystem disagreeing --------------------------------
//
// State records which versions exist; the filesystem holds them. They are
// separate sources of truth and do diverge: a user deletes a directory, a
// removal half-succeeds, a disk fails.

#[test]
fn activating_a_version_whose_files_are_gone_is_refused() {
    // Otherwise `current` names something that cannot be launched, and the
    // failure surfaces later as a confusing error from the launcher.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let installer = Installer::new(&lock);
    let options = InstallOptions { activate: true, ..Default::default() };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();
    install(&lock, dir.path(), &key, "1.1.0", &options).unwrap();

    std::fs::remove_dir_all(paths.version_dir(&v("1.0.0"))).unwrap();
    assert!(!installer.is_usable(&v("1.0.0")));

    let err = installer.activate(&v("1.0.0"), true).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("files are missing"), "got {message}");
    assert!(
        !message.contains("missing is not installed"),
        "the message is mangled by its error variant: {message}"
    );
}

#[test]
fn rollback_skips_a_target_whose_files_are_gone() {
    // Rollback is the recovery mechanism, so it must never recover *into* a
    // broken state.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let installer = Installer::new(&lock);
    let options = InstallOptions { activate: true, ..Default::default() };

    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();
    installer.commit_health().unwrap();
    install(&lock, dir.path(), &key, "1.1.0", &options).unwrap();
    installer.commit_health().unwrap();
    install(&lock, dir.path(), &key, "1.2.0", &options).unwrap();

    // The obvious rollback target loses its files.
    std::fs::remove_dir_all(paths.version_dir(&v("1.1.0"))).unwrap();

    let restored = installer.record_failure("crashed").unwrap();
    assert_eq!(restored, Some(v("1.0.0")), "must skip to a version that is present");
    assert!(
        paths.version_dir(&v("1.0.0")).join("bin/app").is_file(),
        "the restored version must be launchable"
    );

    let state = lock.load_state().unwrap().value;
    assert!(state.is_bad(&v("1.1.0")), "the missing version must be quarantined");
}

#[test]
fn a_failure_with_nothing_to_fall_back_to_is_a_terminal_state() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let installer = Installer::new(&lock);
    let options = InstallOptions { activate: true, ..Default::default() };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();

    assert_eq!(installer.record_failure("the only version is broken").unwrap(), None);
    assert!(
        installer.is_terminal_failure().unwrap(),
        "a caller must be able to detect that nothing automatic can help"
    );
}

#[test]
fn a_version_whose_files_vanished_can_be_reinstalled() {
    // Refusing as "already installed" would leave the user unable to repair
    // their own installation.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let options = InstallOptions { activate: true, ..Default::default() };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();

    std::fs::remove_dir_all(paths.version_dir(&v("1.0.0"))).unwrap();

    install(&lock, dir.path(), &key, "1.0.0", &options)
        .expect("a version with no files must be repairable");
    assert!(paths.version_dir(&v("1.0.0")).join("bin/app").is_file());
}

#[test]
fn reinstalling_an_intact_version_says_so_plainly() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let options = InstallOptions { activate: true, ..Default::default() };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();

    let err = install(&lock, dir.path(), &key, "1.0.0", &options).unwrap_err();
    assert!(
        err.to_string().contains("already installed"),
        "the message must describe the real problem, got {err}"
    );
}

// --- crash recovery ------------------------------------------------------
//
// Simulated by writing the phase a crash would have left, then running the
// next operation and asserting it reaches a launchable state.

#[test]
fn recovery_discards_a_half_written_staging_tree() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let options = InstallOptions { activate: true, ..Default::default() };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();

    // A crash during extraction of 1.1.0.
    let staging = paths.staging_dir(&v("1.1.0"));
    std::fs::create_dir_all(staging.join("partial")).unwrap();
    let mut state = lock.load_state().unwrap().value;
    state.update = UpdatePhase::Installing { version: v("1.1.0") };
    lock.save_state(&state).unwrap();

    let report = Installer::new(&lock).recover().unwrap();
    assert!(report.removed_staging.contains(&"1.1.0".to_string()));
    assert!(!staging.exists());
    // The installation is still launchable on the version it had.
    assert_eq!(lock.load_state().unwrap().value.current_version, Some(v("1.0.0")));
}

#[test]
fn recovery_removes_debris_but_never_an_installed_version() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let options = InstallOptions { activate: true, ..Default::default() };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();

    // A version directory state has never heard of: a crash between promotion
    // and the state write.
    let debris = paths.version_dir(&v("9.9.9"));
    std::fs::create_dir_all(debris.join("stuff")).unwrap();
    let mut state = lock.load_state().unwrap().value;
    state.update = UpdatePhase::Installing { version: v("9.9.9") };
    lock.save_state(&state).unwrap();

    let report = Installer::new(&lock).recover().unwrap();
    assert!(report.removed_debris.contains(&"9.9.9".to_string()));
    assert!(!debris.exists(), "debris must go");
    assert!(paths.version_dir(&v("1.0.0")).exists(), "a real version must never be removed");
}

#[test]
fn recovery_never_removes_a_version_the_state_records() {
    // The dangerous case: a crash recorded `Installing` for a version that is
    // *already installed and working*. Treating it as debris would delete a
    // good version, and if it were the active one the installation would be
    // left unlaunchable.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let options = InstallOptions { activate: true, ..Default::default() };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();

    let mut state = lock.load_state().unwrap().value;
    state.update = UpdatePhase::Installing { version: v("1.0.0") };
    lock.save_state(&state).unwrap();

    let report = Installer::new(&lock).recover().unwrap();
    assert!(report.removed_debris.is_empty(), "a recorded version is not debris: {report:?}");
    assert!(
        paths.version_dir(&v("1.0.0")).join("bin/app").is_file(),
        "the installed version must survive recovery"
    );
    let state = lock.load_state().unwrap().value;
    assert_eq!(state.current_version, Some(v("1.0.0")));
}

#[test]
fn recovery_keeps_a_fully_staged_version() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    install(&lock, dir.path(), &key, "1.0.0", &InstallOptions::default()).unwrap();

    // Extraction completed; the phase says Staged. Discarding it would throw
    // away complete, verified work.
    let report = Installer::new(&lock).recover().unwrap();
    assert!(report.left_in_place);
    assert!(paths.version_dir(&v("1.0.0")).join("bin/app").is_file());

    // The phase itself must survive too, not just the files. It is the only
    // record that a version is staged and waiting to be activated; clearing it
    // leaves the directory on disk with nothing pointing at it.
    let state = lock.load_state().unwrap().value;
    assert!(
        matches!(&state.update, UpdatePhase::Staged { version } if version == &v("1.0.0")),
        "the staged marker was erased: {:?}",
        state.update
    );
}

#[test]
fn repeated_recovery_never_erases_a_staged_marker() {
    // Recovery runs at the start of every operation, so a marker that survives
    // once but not twice is still lost in practice.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    install(&lock, dir.path(), &key, "1.0.0", &InstallOptions::default()).unwrap();

    let installer = Installer::new(&lock);
    for _ in 0..3 {
        installer.recover().unwrap();
    }

    let state = lock.load_state().unwrap().value;
    assert!(matches!(state.update, UpdatePhase::Staged { .. }), "got {:?}", state.update);
}

#[test]
fn recovery_finishes_an_interrupted_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let options = InstallOptions { activate: true, ..Default::default() };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();
    install(&lock, dir.path(), &key, "1.1.0", &options).unwrap();

    // A crash after the rollback phase was recorded but before it completed.
    let mut state = lock.load_state().unwrap().value;
    state.update = UpdatePhase::RollingBack { from: v("1.1.0"), to: v("1.0.0") };
    lock.save_state(&state).unwrap();

    let report = Installer::new(&lock).recover().unwrap();
    assert_eq!(report.completed_rollback, Some("1.0.0".to_string()));

    let state = lock.load_state().unwrap().value;
    assert_eq!(state.current_version, Some(v("1.0.0")), "must end on the rollback target");
    assert!(state.is_bad(&v("1.1.0")));
    assert!(state.update.is_idle());
}

#[test]
fn recovery_never_pre_empts_probation() {
    // Only the launcher may decide whether a probationary version passed.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let options = InstallOptions { activate: true, ..Default::default() };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();
    install(&lock, dir.path(), &key, "1.1.0", &options).unwrap();

    let before = lock.load_state().unwrap().value;
    let report = Installer::new(&lock).recover().unwrap();
    assert!(report.left_in_place);

    let after = lock.load_state().unwrap().value;
    assert_eq!(before.update, after.update, "probation must be untouched");
    assert_eq!(after.current_version, Some(v("1.1.0")));
}

#[test]
fn an_interrupted_install_leaves_a_launchable_installation() {
    // The property that matters most: whatever the crash, the next run ends
    // with a resolvable active version.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let options = InstallOptions { activate: true, ..Default::default() };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();

    for phase in [
        UpdatePhase::Downloading { version: v("1.1.0") },
        UpdatePhase::Verifying { version: v("1.1.0") },
        UpdatePhase::Installing { version: v("1.1.0") },
    ] {
        let mut state = lock.load_state().unwrap().value;
        state.update = phase.clone();
        lock.save_state(&state).unwrap();

        Installer::new(&lock).recover().unwrap();
        let state = lock.load_state().unwrap().value;
        let active = state.active().unwrap_or_else(|e| panic!("{phase:?}: {e}"));
        assert_eq!(*active, v("1.0.0"));
        assert!(paths.version_dir(active).join("bin/app").is_file(), "{phase:?}");
    }
}

#[test]
fn state_written_by_a_crash_is_never_mistaken_for_a_fresh_install() {
    let dir = tempfile::tempdir().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let mut state = InstallState::new("com.example.app");
    state.update = UpdatePhase::Installing { version: v("1.0.0") };
    lock.save_state(&state).unwrap();

    Installer::new(&lock).recover().unwrap();
    assert!(lock.load_state().unwrap().value.update.is_idle());
}

/// Writes a stand-in launcher binary and returns its path.
///
/// The contents are irrelevant to installation, which copies bytes and sets a
/// mode without ever running them.
fn fake_launcher(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("xpack-launcher-source");
    std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
    path
}

#[test]
fn installing_places_the_launcher_in_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let source = fake_launcher(dir.path());
    let options =
        InstallOptions { activate: true, launcher: Some(source.clone()), ..Default::default() };
    let installed = install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();

    assert_eq!(installed.launcher, Some(xpack_install::LauncherOutcome::Installed));

    let launcher = paths.launcher_file_named(&installed_names());
    assert!(launcher.is_file(), "no launcher at {}", launcher.display());
    assert_eq!(std::fs::read(&launcher).unwrap(), std::fs::read(&source).unwrap());

    // The launcher resolves its installation from its own location, so it has
    // to sit directly in the root.
    assert_eq!(launcher.parent().unwrap(), paths.root());
}

#[test]
#[cfg(unix)]
fn the_installed_launcher_is_executable() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let options =
        InstallOptions { launcher: Some(fake_launcher(dir.path())), ..Default::default() };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();

    let mode = std::fs::metadata(paths.launcher_file_named(&installed_names()))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111, "not executable: mode {mode:o}");
}

#[test]
fn a_launcher_already_in_place_is_never_overwritten() {
    // Replacing a resident launcher fails on Windows, and an application
    // update has no reason to: the launcher belongs to xPack, not the version.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let options =
        InstallOptions { launcher: Some(fake_launcher(dir.path())), ..Default::default() };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();

    std::fs::write(paths.launcher_file(), b"an existing launcher").unwrap();

    let second = install(&lock, dir.path(), &key, "1.1.0", &options).unwrap();

    assert_eq!(second.launcher, Some(xpack_install::LauncherOutcome::AlreadyPresent));
    assert_eq!(std::fs::read(paths.launcher_file()).unwrap(), b"an existing launcher");
}

#[test]
fn a_different_launcher_binary_still_does_not_replace_one_in_place() {
    // Directly, without the install machinery: the check is existence, not
    // whether the bytes differ. Installing twice from the same source would
    // pass even if this compared contents, which is not the guarantee the
    // Windows reasoning depends on.
    let dir = tempfile::tempdir().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let installer = Installer::new(&lock);

    std::fs::create_dir_all(paths.root()).unwrap();
    std::fs::write(paths.launcher_file(), b"the resident launcher").unwrap();

    let newer = dir.path().join("newer-launcher");
    std::fs::write(&newer, b"a completely different binary").unwrap();

    let outcome = installer.install_launcher(&newer).unwrap();

    assert_eq!(outcome, xpack_install::LauncherOutcome::AlreadyPresent);
    assert_eq!(std::fs::read(paths.launcher_file()).unwrap(), b"the resident launcher");
}

#[test]
fn installing_without_a_launcher_leaves_the_root_without_one() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let installed = install(&lock, dir.path(), &key, "1.0.0", &InstallOptions::default()).unwrap();

    assert_eq!(installed.launcher, None);
    assert!(!paths.launcher_file().exists());
}

/// Covers the guard only.
///
/// An empty source is rejected before anything is written, so "nothing is left
/// behind" is true here without the cleanup path running at all. The cleanup
/// that matters — the file is written, made executable, and the mode change
/// fails — has no test: provoking a `set_permissions` failure on a file just
/// created in a writable directory is not portable. That path is reasoned, not
/// covered.
#[test]
fn an_empty_launcher_is_refused_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let empty = dir.path().join("empty-launcher");
    std::fs::write(&empty, b"").unwrap();

    let options = InstallOptions { launcher: Some(empty), ..Default::default() };
    let error = install(&lock, dir.path(), &key, "1.0.0", &options).unwrap_err();

    assert!(error.to_string().contains("empty"), "got {error}");
    assert!(!paths.launcher_file().exists());
}

#[test]
fn uninstalling_removes_the_launcher() {
    // The root is emptied non-recursively, so a launcher left behind would
    // keep every uninstall reporting incomplete.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let options =
        InstallOptions { launcher: Some(fake_launcher(dir.path())), ..Default::default() };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();
    let launcher = paths.launcher_file_named(&installed_names());
    assert!(launcher.is_file(), "precondition: a launcher is installed");

    let removal = xpack_install::uninstall(lock).unwrap();

    assert!(removal.is_complete(), "left behind: {:?}", removal.remaining);
    assert!(!launcher.exists());
}

#[test]
fn recovery_leaves_a_download_alone_while_its_lease_is_held() {
    // The download runs with the installation lock released, so recovery in
    // another command sees `Downloading` and would otherwise clear the
    // directory out from under a live writer.
    let dir = tempfile::tempdir().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let mut state = InstallState::new("com.example.app");
    state.update = UpdatePhase::Downloading { version: v("1.1.0") };
    lock.save_state(&state).unwrap();

    let partial = paths.downloads_dir().join("app-1.1.0.xpkg");
    std::fs::create_dir_all(paths.downloads_dir()).unwrap();
    std::fs::write(&partial, b"half a package").unwrap();

    // Held for the duration, exactly as a downloader would.
    let lease = xpack_platform::DownloadLease::acquire(&paths).unwrap();
    assert!(lease.is_some(), "precondition: the lease was free");

    let report = Installer::new(&lock).recover().unwrap();

    assert!(!report.cleared_downloads, "recovery deleted a download in progress");
    assert!(partial.is_file(), "the partial download was removed");
    let state = lock.load_state().unwrap().value;
    assert!(
        matches!(state.update, UpdatePhase::Downloading { .. }),
        "the phase was cleared under a live download: {:?}",
        state.update
    );
}

#[test]
fn recovery_clears_a_download_whose_lease_is_gone() {
    // The other half: once nobody holds the lease, the partial file is debris
    // from a process that died and must not be mistaken for a live download.
    let dir = tempfile::tempdir().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let mut state = InstallState::new("com.example.app");
    state.update = UpdatePhase::Downloading { version: v("1.1.0") };
    lock.save_state(&state).unwrap();

    let partial = paths.downloads_dir().join("app-1.1.0.xpkg");
    std::fs::create_dir_all(paths.downloads_dir()).unwrap();
    std::fs::write(&partial, b"abandoned").unwrap();

    // No lease held: the writer is gone.
    let report = Installer::new(&lock).recover().unwrap();

    assert!(report.cleared_downloads, "abandoned debris was left behind");
    assert!(!partial.exists());
    let state = lock.load_state().unwrap().value;
    assert!(state.update.is_idle(), "got {:?}", state.update);
}

// --- desktop integration ------------------------------------------------
//
// The entry is the only thing xPack writes outside the installation root, so
// these tests pass explicit roots inside a temporary directory. A test that
// used the real ones would leave an entry in the menu of whoever ran the
// suite.

/// Builds a manifest asking for a desktop entry, with an icon in the payload.
fn wants_a_shortcut() -> xpack_core::DesktopSpec {
    xpack_core::DesktopSpec {
        shortcut: true,
        icon: Some("data.txt".into()),
        categories: vec!["Utility".into()],
        terminal: false,
    }
}

#[test]
fn a_package_that_asks_for_no_entry_gets_none() {
    // The default, and the one that must never surprise a user by writing
    // into their application menu.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let desktop = common::desktop_roots(dir.path());
    let options = InstallOptions {
        activate: true,
        desktop_roots: Some(desktop.clone()),
        ..Default::default()
    };
    let installed = install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();

    assert_eq!(installed.desktop, xpack_install::DesktopOutcome::NotRequested);
    assert!(!desktop.data.exists(), "something was written to the data directory");
    assert!(!desktop.home.exists(), "something was written to the home directory");
}

#[test]
fn a_package_that_asks_for_an_entry_gets_one_and_an_icon_beside_the_launcher() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let desktop = common::desktop_roots(dir.path());
    let options = InstallOptions {
        activate: true,
        // An entry is only created when there is a launcher for it to point
        // at; see the test below that asserts the refusal.
        launcher: Some(common::fake_binary(dir.path(), "launcher")),
        desktop_roots: Some(desktop.clone()),
        ..Default::default()
    };

    let package = common::build_package_with(dir.path(), &key, "1.0.0", &wants_a_shortcut());
    let mut verified =
        open_and_verify(&package, &lock, &TrustDecision::Explicit(key.public())).unwrap();
    let installed = Installer::new(&lock).install(&mut verified, &options).unwrap();

    let created = match &installed.desktop {
        xpack_install::DesktopOutcome::Done(paths) => paths.clone(),
        other => panic!("expected an entry to be created, got {other:?}"),
    };
    assert!(!created.is_empty());
    for path in &created {
        assert!(path.exists(), "{} was reported but does not exist", path.display());
    }

    // The icon is copied out of the version directory, because that directory
    // is replaced by the next update and the entry must not point into it.
    let icon = paths.root().join("icon.txt");
    assert!(icon.is_file(), "the icon was not copied into the installation root");
    assert!(!icon.starts_with(paths.versions_dir()));
}

#[test]
fn uninstalling_takes_the_desktop_entry_and_the_icon_with_it() {
    // A stale entry whose target no longer exists is the classic uninstaller
    // failure, so this asserts on absence rather than on a return value.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let desktop = common::desktop_roots(dir.path());

    let created = {
        let lock = InstallLock::acquire(&paths).unwrap();
        let options = InstallOptions {
            activate: true,
            launcher: Some(common::fake_binary(dir.path(), "launcher")),
            desktop_roots: Some(desktop.clone()),
            ..Default::default()
        };
        let package = common::build_package_with(dir.path(), &key, "1.0.0", &wants_a_shortcut());
        let mut verified =
            open_and_verify(&package, &lock, &TrustDecision::Explicit(key.public())).unwrap();
        let installed = Installer::new(&lock).install(&mut verified, &options).unwrap();
        match installed.desktop {
            xpack_install::DesktopOutcome::Done(paths) => paths,
            other => panic!("expected an entry, got {other:?}"),
        }
    };

    let lock = InstallLock::acquire(&paths).unwrap();
    let removal = xpack_install::uninstall_with_roots(lock, Some(&desktop)).unwrap();

    for path in &created {
        assert!(!path.exists(), "{} survived the uninstall", path.display());
    }
    assert!(removal.is_complete(), "the root was not empty afterwards: {:?}", removal.remaining);
}

#[test]
fn an_installation_with_no_launcher_gets_no_desktop_entry() {
    // A shortcut to a missing executable is worse than no shortcut: it looks
    // correct and fails with "no such file" naming a path the user can see.
    // Reachable from `xpack install --no-launcher`, and from the background
    // updater, which supplies no launcher on any staged update.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let desktop = common::desktop_roots(dir.path());
    let options = InstallOptions {
        activate: true,
        desktop_roots: Some(desktop.clone()),
        // No launcher and no windowed launcher, which is what --no-launcher does.
        ..Default::default()
    };

    let package = common::build_package_with(dir.path(), &key, "1.0.0", &wants_a_shortcut());
    let mut verified =
        open_and_verify(&package, &lock, &TrustDecision::Explicit(key.public())).unwrap();
    let installed = Installer::new(&lock).install(&mut verified, &options).unwrap();

    // Reported as a failure, not as "the manifest asked for nothing": the
    // manifest did ask, and the caller is entitled to know it did not happen.
    match &installed.desktop {
        xpack_install::DesktopOutcome::Failed(reason) => {
            assert!(reason.contains("not installed"), "got {reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    // And nothing was written into the user's menu directories.
    assert!(!desktop.data.exists(), "an entry was written to the data directory");
    assert!(!desktop.home.exists(), "an entry was written to the home directory");

    // The installation itself is still perfectly good.
    assert!(installed.activated);
}

#[test]
fn a_users_own_file_in_the_root_is_reported_rather_than_deleted() {
    // `uninstall` promises to remove only what xPack put there. An earlier
    // version of the icon cleanup matched any `icon.*` in the root, which
    // would have deleted a user's own file.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let desktop = common::desktop_roots(dir.path());

    {
        let lock = InstallLock::acquire(&paths).unwrap();
        let options = InstallOptions {
            activate: true,
            launcher: Some(common::fake_binary(dir.path(), "launcher")),
            desktop_roots: Some(desktop.clone()),
            ..Default::default()
        };
        let package = common::build_package_with(dir.path(), &key, "1.0.0", &wants_a_shortcut());
        let mut verified =
            open_and_verify(&package, &lock, &TrustDecision::Explicit(key.public())).unwrap();
        Installer::new(&lock).install(&mut verified, &options).unwrap();
    }

    let theirs = paths.root().join("icon.jpg");
    std::fs::write(&theirs, b"a file the user put here").unwrap();

    let lock = InstallLock::acquire(&paths).unwrap();
    let removal = xpack_install::uninstall_with_roots(lock, Some(&desktop)).unwrap();

    assert!(theirs.is_file(), "a file xPack did not create was deleted");
    assert!(!removal.is_complete(), "the root should not be reported as removed");
    assert!(
        removal.remaining.iter().any(|p| p == &theirs),
        "the leftover was not reported: {:?}",
        removal.remaining
    );

    // The icon xPack itself copied in is gone, though.
    assert!(!paths.root().join("icon.txt").exists(), "xPack's own icon survived");
}

// ---------------------------------------------------------------------------
// Executables named after the application they serve.
// ---------------------------------------------------------------------------

#[test]
fn installing_names_every_executable_after_the_application() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let options = InstallOptions {
        activate: true,
        launcher: Some(common::fake_binary(dir.path(), "src-launcher")),
        updater: Some(common::fake_binary(dir.path(), "src-updater")),
        uninstaller: Some(common::fake_binary(dir.path(), "src-uninstaller")),
        ..Default::default()
    };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();

    let names = installed_names();
    for file in [
        paths.launcher_file_named(&names),
        paths.updater_file_named(&names),
        paths.uninstaller_file_named(&names),
    ] {
        assert!(file.is_file(), "not installed: {}", file.display());
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.contains("Example"), "{name} is not named after the application");
        assert!(!name.contains("xpack"), "{name} still carries xPack's name");
    }
}

#[test]
fn the_name_is_recorded_so_a_later_run_finds_the_same_files() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let options = InstallOptions {
        launcher: Some(common::fake_binary(dir.path(), "l")),
        ..Default::default()
    };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();

    let state = lock.load_state().unwrap().value;
    assert_eq!(state.binary_base_name.as_deref(), Some("Example"));
    assert_eq!(state.binary_names(), installed_names());
}

#[test]
fn an_installation_that_already_carries_xpack_names_keeps_them() {
    // The migration case, and the one that matters most: these files exist on
    // a user's machine right now. Renaming them would orphan the shortcut
    // pointing at one and, on Windows, cannot be done at all while the
    // launcher is the resident process watching the application start.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    std::fs::create_dir_all(paths.root()).unwrap();
    std::fs::write(paths.launcher_file(), b"an older launcher").unwrap();

    let options = InstallOptions {
        launcher: Some(common::fake_binary(dir.path(), "l")),
        ..Default::default()
    };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();

    let state = lock.load_state().unwrap().value;
    assert_eq!(state.binary_base_name, None, "an existing installation must not be renamed");
    assert_eq!(std::fs::read(paths.launcher_file()).unwrap(), b"an older launcher");
    assert!(
        !paths.launcher_file_named(&installed_names()).exists(),
        "a second launcher was written beside the one already there"
    );
}

#[test]
fn renaming_the_application_leaves_the_executables_where_they_are() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let options = InstallOptions {
        launcher: Some(common::fake_binary(dir.path(), "l")),
        ..Default::default()
    };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();

    // The same application, released under a new display name.
    let renamed = common::build_package_named(
        dir.path(),
        &key,
        "1.1.0",
        "Renamed Entirely",
        &xpack_core::DesktopSpec::default(),
    );
    let mut verified =
        open_and_verify(&renamed, &lock, &TrustDecision::Explicit(key.public())).unwrap();
    Installer::new(&lock).install(&mut verified, &options).unwrap();

    assert!(paths.launcher_file_named(&installed_names()).is_file(), "the original file moved");
    let after = xpack_core::BinaryNames::from_display_name("Renamed Entirely");
    assert!(
        !paths.launcher_file_named(&after).exists(),
        "a rename wrote a second launcher under the new name"
    );
}

#[test]
fn uninstalling_removes_executables_named_after_the_application() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let options = InstallOptions {
        launcher: Some(common::fake_binary(dir.path(), "src-launcher")),
        updater: Some(common::fake_binary(dir.path(), "src-updater")),
        uninstaller: Some(common::fake_binary(dir.path(), "src-uninstaller")),
        ..Default::default()
    };
    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();

    let names = installed_names();
    let files = [
        paths.launcher_file_named(&names),
        paths.updater_file_named(&names),
        paths.uninstaller_file_named(&names),
    ];
    assert!(files.iter().all(|f| f.is_file()), "precondition: all three are installed");

    let removal = xpack_install::uninstall(lock).unwrap();

    assert!(removal.is_complete(), "left behind: {:?}", removal.remaining);
    for file in files {
        assert!(!file.exists(), "left behind: {}", file.display());
    }
}

#[test]
fn a_desktop_entry_points_at_the_launcher_that_was_actually_installed() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let desktop = xpack_core::DesktopSpec { shortcut: true, ..Default::default() };
    let package = common::build_package_with(dir.path(), &key, "1.0.0", &desktop);
    let mut verified =
        open_and_verify(&package, &lock, &TrustDecision::Explicit(key.public())).unwrap();

    let options = InstallOptions {
        activate: true,
        launcher: Some(common::fake_binary(dir.path(), "src-launcher")),
        gui_launcher: Some(common::fake_binary(dir.path(), "src-launcherw")),
        desktop_roots: Some(common::desktop_roots(dir.path())),
        ..Default::default()
    };
    Installer::new(&lock).install(&mut verified, &options).unwrap();

    // An entry naming a file that is not there looks correct and fails with
    // "no such file", which is worse than having no entry at all.
    let target = paths.shortcut_target_named(&installed_names());
    assert!(target.is_file(), "the entry would point at nothing: {}", target.display());
}

/// Installs a package that asks for a desktop entry, with the user's answer.
fn install_asking_for_an_entry(
    dir: &std::path::Path,
    key: &KeyPair,
    version: &str,
    desktop_entry: Option<bool>,
) -> xpack_core::Result<xpack_install::Installed> {
    let lock = InstallLock::acquire(&install_paths(dir)).unwrap();
    let options = InstallOptions {
        activate: true,
        launcher: Some(common::fake_binary(dir, "launcher")),
        desktop_roots: Some(common::desktop_roots(dir)),
        desktop_entry,
        ..Default::default()
    };
    let package = common::build_package_with(dir, key, version, &wants_a_shortcut());
    let mut verified = open_and_verify(&package, &lock, &TrustDecision::Explicit(key.public()))?;
    Installer::new(&lock).install(&mut verified, &options)
}

#[test]
fn a_declined_entry_is_not_written_and_later_updates_do_not_add_it() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let desktop = common::desktop_roots(dir.path());

    let first = install_asking_for_an_entry(dir.path(), &key, "1.0.0", Some(false)).unwrap();
    assert_eq!(first.desktop, xpack_install::DesktopOutcome::NotRequested);
    assert!(install_paths(dir.path()).desktop_preference_file().is_file(), "choice not recorded");

    // An update asks nothing, exactly as the background updater does, and
    // the package still asks for an entry. The recorded choice wins.
    let update = install_asking_for_an_entry(dir.path(), &key, "2.0.0", None).unwrap();
    assert_eq!(update.desktop, xpack_install::DesktopOutcome::NotRequested);

    assert!(!desktop.data.exists(), "an entry was written to the data directory");
    assert!(!desktop.home.exists(), "an entry was written to the home directory");
}

#[test]
fn declining_on_an_existing_installation_is_refused_before_anything_changes() {
    // Its updater may predate the recorded choice and would put the entry
    // back, so the choice is refused rather than silently undone later.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    install_asking_for_an_entry(dir.path(), &key, "1.0.0", None).unwrap();

    let error =
        install_asking_for_an_entry(dir.path(), &key, "2.0.0", Some(false)).expect_err("refused");
    assert!(error.to_string().contains("first installation"), "{error}");

    assert!(!paths.desktop_preference_file().exists(), "the choice was recorded anyway");
    assert!(!paths.version_dir(&v("2.0.0")).exists(), "the version was installed anyway");
    let state = InstallLock::acquire(&paths).unwrap().load_state().unwrap().value;
    assert_eq!(state.current_version, Some(v("1.0.0")));
}

#[test]
fn saying_yes_never_adds_an_entry_the_package_did_not_ask_for() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let desktop = common::desktop_roots(dir.path());
    let lock = InstallLock::acquire(&install_paths(dir.path())).unwrap();

    let options = InstallOptions {
        activate: true,
        launcher: Some(common::fake_binary(dir.path(), "launcher")),
        desktop_roots: Some(desktop.clone()),
        desktop_entry: Some(true),
        ..Default::default()
    };
    let installed = install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();

    assert_eq!(installed.desktop, xpack_install::DesktopOutcome::NotRequested);
    assert!(!desktop.data.exists() && !desktop.home.exists());
}

#[test]
fn saying_yes_on_an_existing_installation_keeps_its_entry() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    install_asking_for_an_entry(dir.path(), &key, "1.0.0", None).unwrap();

    let update = install_asking_for_an_entry(dir.path(), &key, "2.0.0", Some(true)).unwrap();
    assert!(
        matches!(update.desktop, xpack_install::DesktopOutcome::Done(_)),
        "got {:?}",
        update.desktop
    );
}

#[test]
fn an_unreadable_choice_counts_as_declined() {
    // The only record ever written says no. Guessing yes would write into a
    // user's menu against what they most likely chose.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    install_asking_for_an_entry(dir.path(), &key, "1.0.0", Some(false)).unwrap();
    std::fs::write(install_paths(dir.path()).desktop_preference_file(), b"garbage").unwrap();

    let update = install_asking_for_an_entry(dir.path(), &key, "2.0.0", None).unwrap();
    assert_eq!(update.desktop, xpack_install::DesktopOutcome::NotRequested);
}

// --- finding an installation that is not in the default place ---------------
//
// Not on Windows, where the record is the user's real registry, which a test
// has no business writing into; the reading there is compiled, not run here.

#[cfg(not(windows))]
#[test]
fn an_installation_is_found_through_its_desktop_entry() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let desktop = common::desktop_roots(dir.path());

    install_asking_for_an_entry(dir.path(), &key, "1.0.0", None).unwrap();

    // `dir` is the root `install_paths` builds on: the one `--root` names.
    let found =
        xpack_install::integration::recorded_installation("com.example.app", "Example", &desktop);
    assert_eq!(found.as_deref(), Some(dir.path()));
}

#[cfg(not(windows))]
#[test]
fn an_entry_whose_installation_is_gone_is_not_believed() {
    // An entry outliving its installation — removed by hand, or restored from
    // a backup — must not send the next install somewhere that is not there.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let desktop = common::desktop_roots(dir.path());
    install_asking_for_an_entry(dir.path(), &key, "1.0.0", None).unwrap();

    std::fs::remove_dir_all(install_paths(dir.path()).state_dir()).unwrap();

    let found =
        xpack_install::integration::recorded_installation("com.example.app", "Example", &desktop);
    assert_eq!(found, None);
}

#[cfg(not(windows))]
#[test]
fn an_entry_naming_another_applications_directory_is_not_believed() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let desktop = common::desktop_roots(dir.path());
    install_asking_for_an_entry(dir.path(), &key, "1.0.0", None).unwrap();

    // The same entry, asked about under another application's id.
    let found =
        xpack_install::integration::recorded_installation("com.example.other", "Example", &desktop);
    assert_eq!(found, None);
}

#[cfg(not(windows))]
#[test]
fn no_entry_means_nothing_is_found() {
    let dir = tempfile::tempdir().unwrap();
    let found = xpack_install::integration::recorded_installation(
        "com.example.app",
        "Example",
        &common::desktop_roots(dir.path()),
    );
    assert_eq!(found, None);
}

#[test]
fn a_decline_left_by_a_failed_first_attempt_does_not_bind_the_next_one() {
    // The first attempt declined, recorded it, and then failed before any
    // version was installed. The next first attempt did not decline.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    std::fs::write(paths.desktop_preference_file(), r#"{"formatVersion":1,"entry":false}"#)
        .unwrap();

    let installed = install_asking_for_an_entry(dir.path(), &key, "1.0.0", None).unwrap();

    assert!(
        matches!(installed.desktop, xpack_install::DesktopOutcome::Done(_)),
        "got {:?}",
        installed.desktop
    );
    assert!(!paths.desktop_preference_file().exists());
}

/// A real, verified package whose manifest claims more than the disk holds.
///
/// The claim cannot be made through a signed package without actually
/// shipping that many bytes, so the test wraps the verified source and
/// changes only the figure the installer checks; extraction must never start.
struct Oversized {
    inner: xpack_package::VerifiedPackage,
    manifest: xpack_core::Manifest,
}

impl xpack_install::InstallSource for Oversized {
    fn manifest(&self) -> &xpack_core::Manifest {
        &self.manifest
    }
    fn manifest_bytes(&self) -> &[u8] {
        xpack_install::InstallSource::manifest_bytes(&self.inner)
    }
    fn signature(&self) -> &xpack_security::Signature {
        xpack_install::InstallSource::signature(&self.inner)
    }
    fn materialise(
        &mut self,
        _: &std::path::Path,
        _: &dyn xpack_core::progress::ProgressReporter,
    ) -> xpack_core::Result<()> {
        panic!("extraction started on a disk that could not hold the version");
    }
}

#[test]
fn a_version_the_disk_cannot_hold_is_refused_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();

    let package = build_package(dir.path(), &key, "1.0.0");
    let inner = open_and_verify(&package, &lock, &TrustDecision::Explicit(key.public())).unwrap();
    let mut manifest = xpack_install::InstallSource::manifest(&inner).clone();
    let free = xpack_platform::available_space(dir.path()).unwrap();
    manifest.payload.total_size = free.saturating_add(1 << 30);
    let mut source = Oversized { inner, manifest };

    let options = InstallOptions { activate: true, ..Default::default() };
    let error = Installer::new(&lock).install_from(&mut source, &options).unwrap_err();

    match &error {
        xpack_core::Error::NotEnoughSpace { needed, available, .. } => {
            assert!(needed > available, "{error}");
        }
        other => panic!("expected NotEnoughSpace, got {other}"),
    }
    assert!(error.to_string().contains("not enough free space"), "{error}");
    assert!(!paths.version_dir(&v("1.0.0")).exists(), "a version directory was written");
    assert!(
        lock.load_or_new_state("com.example.app").unwrap().current_version.is_none(),
        "the refused version was recorded"
    );
}
