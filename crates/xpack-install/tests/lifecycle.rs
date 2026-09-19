//! Install, activate, roll back and recover, against real signed packages.

mod common;

use common::{build_package, install_paths};
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

    let allowed = InstallOptions { activate: true, allow_downgrade: true };
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
fn uninstalling_removes_versions_and_clears_state() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = install_paths(dir.path());
    let lock = InstallLock::acquire(&paths).unwrap();
    let options = InstallOptions { activate: true, ..Default::default() };

    install(&lock, dir.path(), &key, "1.0.0", &options).unwrap();
    Installer::new(&lock).uninstall().unwrap();

    assert!(!paths.versions_dir().exists());
    let state = lock.load_state().unwrap().value;
    assert!(state.current_version.is_none());
    assert!(state.versions.is_empty());
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
    assert!(err.to_string().contains("files are missing"), "got {err}");
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
