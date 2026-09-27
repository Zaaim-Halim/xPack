//! Locking an application with a password, through the real binaries.
//!
//! `protection` in `xpack.json`: `installer` makes the installer ask for the
//! password; `packages` also seals every package, so one taken from the update
//! server cannot be installed without it. Each test performs what the lock is
//! for, or what it must refuse.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const PASSWORD: &str = "correct horse battery staple";
/// A string only the application's payload contains, to look for elsewhere.
const MARKER: &str = "THE-APPLICATION'S-OWN-SECRET-MARKER-9f3b";

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

struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    /// A project whose `xpack.json` has this `protection`.
    fn new(protection: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("payload/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let name = format!("app{}", std::env::consts::EXE_SUFFIX);
        std::fs::copy(env!("CARGO_BIN_EXE_xpack-test-payload"), bin.join(&name)).unwrap();
        std::fs::write(dir.path().join("payload/secret.txt"), MARKER.repeat(50)).unwrap();
        let project = Self { dir };
        project.config("1.0.0", protection);
        assert!(project.xpack(&["keygen", "--out", "signing.json"], None).status.success());
        project
    }

    fn config(&self, version: &str, protection: &str) {
        let name = format!("app{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(
            self.dir.path().join("xpack.json"),
            format!(
                r#"{{"application":{{"id":"com.example.locked","name":"Locked","version":"{version}"}},
                    "launch":{{"executable":"bin/{name}",
                               "environment":{{"XPACK_TEST_PAYLOAD_EXIT":"0",
                                              "XPACK_TEST_PAYLOAD_VERSION":"{version}"}}}},
                    "protection":{protection}}}"#
            ),
        )
        .unwrap();
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Runs `xpack`, with `password` in `XPACK_PASSWORD` when given.
    fn xpack(&self, args: &[&str], password: Option<&str>) -> Output {
        let mut command = Command::new(xpack());
        command.current_dir(self.dir.path()).args(args).env_remove("XPACK_PASSWORD");
        if let Some(password) = password {
            command.env("XPACK_PASSWORD", password);
        }
        command.output().unwrap()
    }

    fn pack(&self, version: &str, password: Option<&str>) -> Output {
        let out = format!("locked-{version}.xpkg");
        self.xpack(&["pack", "payload", "--key", "signing.json", "--out", &out], password)
    }
}

fn contains_marker(path: &Path) -> bool {
    let bytes = std::fs::read(path).unwrap();
    bytes.windows(MARKER.len()).any(|window| window == MARKER.as_bytes())
}

#[test]
fn sealing_packages_without_locking_the_installer_is_refused() {
    let project = Project::new(r#"{"installer":false,"packages":true}"#);
    let out = project.pack("1.0.0", Some(PASSWORD));
    assert!(!out.status.success());
    assert!(text(&out).contains("need the installer lock"), "{}", text(&out));
}

#[test]
fn nothing_is_locked_unless_asked() {
    let project = Project::new("{}");
    assert!(project.pack("1.0.0", None).status.success());
    assert!(!xpack_security::seal::is_sealed(&project.path("locked-1.0.0.xpkg")).unwrap());
}

#[test]
fn a_sealed_package_hides_the_application_and_installs_only_with_the_password() {
    let project = Project::new(r#"{"installer":true,"packages":true}"#);

    let short = project.pack("1.0.0", Some("short"));
    assert!(!short.status.success(), "a short password was accepted");
    let none = project.pack("1.0.0", None);
    assert!(!none.status.success(), "packed with no password");

    assert!(project.pack("1.0.0", Some(PASSWORD)).status.success());
    let package = project.path("locked-1.0.0.xpkg");
    assert!(xpack_security::seal::is_sealed(&package).unwrap());
    assert!(!contains_marker(&package), "the application shows through the seal");

    let root = project.path("root");
    let root = root.to_str().unwrap();
    let args = ["--root", root, "install", "locked-1.0.0.xpkg", "--trust", "signing.pub.json"];
    let without = project.xpack(&args, None);
    assert!(!without.status.success(), "installed without the password");
    let wrong = project.xpack(&args, Some("not the password at all"));
    assert!(!wrong.status.success(), "installed with the wrong password");

    let right = project.xpack(&args, Some(PASSWORD));
    assert!(right.status.success(), "{}", text(&right));
    let paths = xpack_core::InstallPaths::new(Path::new(root), "com.example.locked").unwrap();
    assert!(paths.seal_key_file().is_file(), "no key kept for sealed updates");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(paths.seal_key_file()).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the key is readable by others: {mode:o}");
    }
}

#[test]
fn a_delta_between_sealed_releases_is_sealed_too() {
    let project = Project::new(r#"{"installer":true,"packages":true}"#);
    assert!(project.pack("1.0.0", Some(PASSWORD)).status.success());
    project.config("1.1.0", r#"{"installer":true,"packages":true}"#);
    std::fs::write(project.path("payload/new.txt"), "a change").unwrap();
    assert!(project.pack("1.1.0", Some(PASSWORD)).status.success());

    let out = project.xpack(
        &["delta", "locked-1.0.0.xpkg", "locked-1.1.0.xpkg", "--out", "locked.delta"],
        Some(PASSWORD),
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(xpack_security::seal::is_sealed(&project.path("locked.delta")).unwrap());
}

/// Builds the installer for `package`, returning the program to run.
fn installer(project: &Project, package: &str, password: Option<&str>) -> PathBuf {
    let out = project.xpack(
        &["installer", package, "--config", "xpack.json", "--out", "Locked-installer", "--json"],
        password,
    );
    assert!(out.status.success(), "{}", text(&out));
    let bundle = project.path("Locked-installer/Contents/MacOS");
    if bundle.is_dir() {
        std::fs::read_dir(bundle).unwrap().next().unwrap().unwrap().path()
    } else {
        project.path("Locked-installer")
    }
}

fn run_installer(program: &Path, root: &Path, password: Option<&str>) -> Output {
    let mut command = Command::new(program);
    command
        .args(["--silent", "--root"])
        .arg(root)
        .env_remove("XPACK_INSTALLER_PASSWORD")
        .stdin(Stdio::null());
    if let Some(password) = password {
        command.env("XPACK_INSTALLER_PASSWORD", password);
    }
    command.output().unwrap()
}

#[test]
fn a_locked_installer_installs_nothing_without_the_password() {
    // The installer lock alone: the package is plain, the installer seals it.
    let project = Project::new(r#"{"installer":true}"#);
    assert!(project.pack("1.0.0", None).status.success());
    let program = installer(&project, "locked-1.0.0.xpkg", Some(PASSWORD));

    let payload = if project.path("Locked-installer/Contents").is_dir() {
        project.path("Locked-installer/Contents/Resources/xpack-payload.bundle")
    } else {
        program.clone()
    };
    assert!(!contains_marker(&payload), "the installer carries the application readable");

    let root = project.path("root");
    let paths = xpack_core::InstallPaths::new(&root, "com.example.locked").unwrap();

    let none = run_installer(&program, &root, None);
    assert!(!none.status.success(), "{}", text(&none));
    assert!(text(&none).contains("locked with a password"), "{}", text(&none));
    let wrong = run_installer(&program, &root, Some("not the password at all"));
    assert_eq!(wrong.status.code(), Some(3), "{}", text(&wrong));
    assert!(!paths.state_file().exists(), "something was installed");

    let right = run_installer(&program, &root, Some(PASSWORD));
    assert!(right.status.success(), "{}", text(&right));
    assert!(paths.state_file().is_file());
}
