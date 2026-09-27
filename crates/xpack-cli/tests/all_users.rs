//! `xpack install --all-users`: an installation for every user of the machine.
//!
//! Most of this needs administrator rights, and writes where other programs
//! go, so it runs only as root (`sudo cargo test -p xpack-cli --test
//! all_users`, which CI does) and says so otherwise. The one thing an
//! ordinary user can check is that it is refused to them.
//!
//! As root it performs the attacks the elevated install has to withstand: a
//! planted `XPACK_INSTALL_ROOT` that would steer it into a directory the user
//! controls (refused, with nothing written), a permissive umask that would
//! leave its files writable by everyone, and trusting whatever key signed a
//! package handed to it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ID: &str = "com.example.allusers-test";
const NAME: &str = "xPack All Users Test";

fn xpack() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_xpack"))
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// A signed package in `dir`, and its public key.
fn package(dir: &Path) -> (PathBuf, PathBuf) {
    let bin = dir.join("payload/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let name = format!("app{}", std::env::consts::EXE_SUFFIX);
    std::fs::copy(env!("CARGO_BIN_EXE_xpack-test-payload"), bin.join(&name)).unwrap();
    std::fs::write(
        dir.join("xpack.json"),
        format!(
            r#"{{"application":{{"id":"{ID}","name":"{NAME}","version":"1.0.0"}},
                "launch":{{"executable":"bin/{name}",
                           "environment":{{"XPACK_TEST_PAYLOAD_EXIT":"0",
                                          "XPACK_TEST_PAYLOAD_VERSION":"1.0.0"}}}}}}"#
        ),
    )
    .unwrap();
    let run = |args: &[&str]| {
        let out = Command::new(xpack()).current_dir(dir).args(args).output().unwrap();
        assert!(out.status.success(), "{}", text(&out));
    };
    run(&["keygen", "--out", "signing.json"]);
    run(&["pack", "payload", "--key", "signing.json", "--out", "app.xpkg"]);
    (dir.join("app.xpkg"), dir.join("signing.pub.json"))
}

fn machine_dir() -> PathBuf {
    xpack_install::integration::machine_application_dir(ID, NAME).unwrap()
}

#[test]
fn an_ordinary_user_cannot_install_for_everyone() {
    if xpack_platform::is_elevated() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (package, key) = package(dir.path());

    let out = Command::new(xpack())
        .args(["install", "--all-users", "--trust"])
        .arg(&key)
        .arg(&package)
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert!(text(&out).contains("administrator rights"), "{}", text(&out));
    assert!(!machine_dir().exists(), "something was written for everyone");
}

#[test]
fn trusting_whatever_key_signed_the_package_is_refused_for_everyone() {
    let dir = tempfile::tempdir().unwrap();
    let (package, _) = package(dir.path());
    let out = Command::new(xpack())
        .args(["install", "--all-users", "--trust-on-first-use"])
        .arg(&package)
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    // Refused before anything is read. Not checked by looking for the
    // installation: where the suite runs elevated, the test that installs for
    // everyone may be making it at this moment.
    assert!(text(&out).contains("--trust-on-first-use"), "{}", text(&out));
}

#[test]
fn an_installation_for_everyone_goes_where_programs_go_and_nobody_else_can_change_it() {
    if !xpack_platform::is_elevated() {
        // CI sets this where the test must run, so a skip cannot pass there
        // unnoticed.
        assert!(
            std::env::var_os("XPACK_TEST_REQUIRE_ELEVATED").is_none_or(|value| value.is_empty()),
            "this run was meant to have administrator rights and does not"
        );
        eprintln!("skipped: needs administrator rights (run with sudo)");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (package, key) = package(dir.path());
    let planted = dir.path().join("planted-root");
    let target = machine_dir();
    assert!(!target.exists(), "{} is left from an earlier run", target.display());

    // An installation root planted in the environment is refused, and
    // nothing is written anywhere.
    let planted_run = Command::new(xpack())
        .args(["install", "--all-users", "--trust"])
        .arg(&key)
        .arg(&package)
        .env("XPACK_INSTALL_ROOT", &planted)
        .output()
        .unwrap();
    assert!(!planted_run.status.success(), "{}", text(&planted_run));
    assert!(!planted.exists() && !target.exists(), "the planted root was acted on");

    // A umask that would make every file writable by everyone.
    let mut install = if cfg!(unix) {
        let mut sh = Command::new("/bin/sh");
        sh.arg("-c").arg("umask 000; exec \"$@\"").arg("sh").arg(xpack());
        sh
    } else {
        Command::new(xpack())
    };
    let out = install
        .args(["install", "--all-users", "--trust"])
        .arg(&key)
        .arg(&package)
        .env_remove("XPACK_INSTALL_ROOT")
        .output()
        .unwrap();
    let installed = out.status.success();

    let result = std::panic::catch_unwind(|| {
        assert!(installed, "{}", text(&out));
        assert!(target.join("state").join("state.json").is_file(), "{}", text(&out));
        let state: xpack_core::InstallState =
            xpack_core::store::load(&target.join("state").join("state.json")).unwrap().value;
        assert_eq!(state.scope, xpack_core::InstallScope::Machine);
        assert_eq!(state.application_id, ID);
        #[cfg(unix)]
        assert_nothing_writable_by_others(&target);
    });

    // Removed however the checks went, with the uninstaller it was given, or
    // by hand where there is none.
    let paths = xpack_core::InstallPaths::open(&target);
    let names = xpack_core::BinaryNames::from_display_name(NAME);
    let uninstaller = paths.uninstaller_file_named(&names);
    if uninstaller.is_file() {
        let _ = Command::new(&uninstaller).arg("--yes").output();
    }
    let _ = std::fs::remove_dir_all(&target);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// Fails on any file or directory under `dir` that a user other than its
/// owner could write to.
#[cfg(unix)]
fn assert_nothing_writable_by_others(dir: &Path) {
    use std::os::unix::fs::MetadataExt;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let meta = std::fs::symlink_metadata(&next).unwrap();
        if meta.file_type().is_symlink() {
            continue;
        }
        assert_eq!(meta.mode() & 0o022, 0, "{} is writable by others", next.display());
        assert_eq!(meta.uid(), 0, "{} does not belong to root", next.display());
        if meta.is_dir() {
            pending.extend(std::fs::read_dir(&next).unwrap().map(|e| e.unwrap().path()));
        }
    }
}
