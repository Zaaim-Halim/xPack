//! Looking at an installation, against real installs of real signed packages.
//!
//! Each verdict is checked against what installing then actually does, because
//! the whole point of looking first is that the two never disagree.

mod common;

use std::path::Path;
use std::process::Command;

use common::{build_package, install_paths};
use xpack_core::{InstallPaths, InstallState, Version};
use xpack_install::{Existing, InstallOptions, Installer, TrustDecision, inspect, open_and_verify};
use xpack_platform::InstallLock;
use xpack_security::KeyPair;

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

/// Installs `version`, releasing the lock afterwards as a real installer does.
fn install(root: &Path, key: &KeyPair, version: &str, activate: bool) -> xpack_core::Result<()> {
    let paths = install_paths(root);
    let lock = InstallLock::acquire(&paths)?;
    let package = build_package(root, key, version);
    let mut verified = open_and_verify(&package, &lock, &TrustDecision::Explicit(key.public()))?;
    let options = InstallOptions { activate, ..Default::default() };
    Installer::new(&lock).install(&mut verified, &options).map(drop)
}

/// Every file and directory under `root`, for asserting nothing was created.
fn tree(root: &Path) -> Vec<std::path::PathBuf> {
    if !root.exists() {
        return Vec::new();
    }
    let mut found: Vec<_> =
        walk(root).into_iter().map(|p| p.strip_prefix(root).unwrap().to_path_buf()).collect();
    found.sort();
    found
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        }
        out.push(path);
    }
    out
}

#[test]
fn an_empty_folder_holds_nothing_and_looking_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let paths = install_paths(dir.path());

    assert_eq!(inspect(&paths, &v("1.0.0")), Existing::Nothing);
    assert!(inspect(&paths, &v("1.0.0")).is_first_install());
    assert!(tree(dir.path()).is_empty(), "looking wrote {:?}", tree(dir.path()));
}

#[test]
fn looking_at_an_existing_installation_changes_nothing_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    install(dir.path(), &key, "1.0.0", true).unwrap();
    let paths = install_paths(dir.path());

    // The lock file an install leaves is removed, so that looking would have
    // to create one to take the lock the ordinary way.
    std::fs::remove_file(paths.lock_file()).unwrap();
    let before = tree(dir.path());
    let before_state = std::fs::read(paths.state_file()).unwrap();

    inspect(&paths, &v("2.0.0"));

    assert_eq!(tree(dir.path()), before);
    assert_eq!(std::fs::read(paths.state_file()).unwrap(), before_state);
}

#[test]
fn each_verdict_matches_what_installing_then_does() {
    // Built from real states, then the install is attempted: a verdict that
    // allows must be followed by a success, one that refuses by a refusal.
    struct Case {
        name: &'static str,
        setup: fn(&Path, &KeyPair),
        candidate: &'static str,
        expected: Existing,
    }

    let key = KeyPair::generate().unwrap();
    let cases = [
        Case { name: "fresh", setup: |_, _| {}, candidate: "1.0.0", expected: Existing::Nothing },
        Case {
            name: "upgrade",
            setup: |root, key| install(root, key, "1.0.0", true).unwrap(),
            candidate: "2.0.0",
            expected: Existing::Older(v("1.0.0")),
        },
        Case {
            name: "same version",
            setup: |root, key| install(root, key, "1.0.0", true).unwrap(),
            candidate: "1.0.0",
            expected: Existing::Installed,
        },
        Case {
            name: "downgrade",
            setup: |root, key| install(root, key, "2.0.0", true).unwrap(),
            candidate: "1.0.0",
            expected: Existing::Newer(v("2.0.0")),
        },
        Case {
            name: "inactive",
            setup: |root, key| install(root, key, "1.0.0", false).unwrap(),
            candidate: "2.0.0",
            expected: Existing::Inactive,
        },
        Case {
            name: "damaged",
            setup: |root, key| {
                install(root, key, "1.0.0", true).unwrap();
                let paths = install_paths(root);
                std::fs::remove_dir_all(paths.version_dir(&v("1.0.0"))).unwrap();
            },
            candidate: "1.0.0",
            expected: Existing::Damaged,
        },
    ];

    for case in cases {
        let dir = tempfile::tempdir().unwrap();
        (case.setup)(dir.path(), &key);
        let paths = install_paths(dir.path());

        let verdict = inspect(&paths, &v(case.candidate));
        assert_eq!(verdict, case.expected, "{}", case.name);

        let result = install(dir.path(), &key, case.candidate, true);
        assert_eq!(
            result.is_ok(),
            verdict.allows_install(),
            "{}: inspect said {verdict:?}, install said {result:?}",
            case.name
        );
    }
}

#[test]
fn a_corrupt_installation_is_unreadable_rather_than_fresh() {
    // Reporting "nothing here" would invite an install over a real
    // installation whose records are damaged.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    install(dir.path(), &key, "1.0.0", true).unwrap();
    let paths = install_paths(dir.path());

    std::fs::write(paths.state_file(), b"corrupt").unwrap();
    std::fs::write(xpack_core::store::backup_path(&paths.state_file()), b"corrupt too").unwrap();

    assert!(matches!(inspect(&paths, &v("2.0.0")), Existing::Unreadable(_)));
}

#[test]
fn another_applications_state_is_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let paths = install_paths(dir.path());
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    InstallState::new("com.someone.else").save(&paths.state_file()).unwrap();

    match inspect(&paths, &v("1.0.0")) {
        Existing::Unreadable(message) => assert!(message.contains("com.someone.else"), "{message}"),
        other => panic!("expected unreadable, got {other:?}"),
    }
}

// --- a lock held by another process ----------------------------------------

const CHILD_ROLE: &str = "XPACK_TEST_INSPECT_ROLE";
const CHILD_DIR: &str = "XPACK_TEST_INSPECT_DIR";
const CHILD_SAW_BUSY: i32 = 30;
const CHILD_SAW_OTHER: i32 = 31;

/// The child half: look at the installation and report the verdict.
fn run_child_if_selected() -> bool {
    if std::env::var(CHILD_ROLE).as_deref() != Ok("inspect") {
        return false;
    }
    let root = std::env::var(CHILD_DIR).expect("child directory");
    let paths = InstallPaths::new(Path::new(&root), "com.example.app").unwrap();
    let code = match inspect(&paths, &v("2.0.0")) {
        Existing::Busy => CHILD_SAW_BUSY,
        _ => CHILD_SAW_OTHER,
    };
    std::process::exit(code);
}

#[test]
fn an_installation_another_process_holds_is_busy() {
    if run_child_if_selected() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    install(dir.path(), &key, "1.0.0", true).unwrap();

    let held = InstallLock::acquire(&install_paths(dir.path())).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "an_installation_another_process_holds_is_busy", "--nocapture"])
        .env(CHILD_ROLE, "inspect")
        .env(CHILD_DIR, dir.path())
        .status()
        .unwrap();
    drop(held);

    assert_eq!(status.code(), Some(CHILD_SAW_BUSY));

    // And once it is released, the same installation is simply older.
    assert_eq!(inspect(&install_paths(dir.path()), &v("2.0.0")), Existing::Older(v("1.0.0")));
}
