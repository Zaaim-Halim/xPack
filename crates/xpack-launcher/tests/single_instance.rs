//! One running copy of an application, with the real launcher binary.
//!
//! Each test installs a package whose program records every start and then
//! stays running, puts the launcher binary in the installation as an install
//! does, and starts it the way a user would, more than once.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use xpack_core::manifest::{Application, FormatVersion, LaunchSpec, PayloadSpec, UpdateSpec};
use xpack_core::{InstallPaths, InstanceSpec, Manifest, Platform, Version};
use xpack_install::{InstallOptions, Installer, TrustDecision, open_and_verify};
use xpack_package::PackageBuilder;
use xpack_platform::InstallLock;
use xpack_security::KeyPair;

/// Appends one line per start, with the arguments and the inbox it was told
/// about, then keeps running until killed.
const APP: &str = "#!/bin/sh\n\
    printf 'start %s|%s\\n' \"$*\" \"$XPACK_INSTANCE_INBOX\" >> \"$XPACK_APPLICATION_DIR/starts.log\"\n\
    exec sleep 30\n";

struct World {
    _dir: tempfile::TempDir,
    paths: InstallPaths,
    launcher: PathBuf,
    running: Vec<Child>,
}

impl World {
    fn new(single: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let key = KeyPair::generate().unwrap();
        let payload = dir.path().join("payload");
        fs::create_dir_all(payload.join("bin")).unwrap();
        fs::write(payload.join("bin/app"), APP).unwrap();
        fs::set_permissions(payload.join("bin/app"), fs::Permissions::from_mode(0o755)).unwrap();

        let mut manifest = Manifest {
            format_version: FormatVersion::CURRENT,
            application: Application {
                id: "com.example.app".into(),
                name: "Example".into(),
                version: Version::parse("1.0.0").unwrap(),
                description: None,
                publisher: None,
            },
            platform: Platform::host().unwrap(),
            launch: LaunchSpec {
                executable: "bin/app".into(),
                arguments: vec![],
                working_directory: None,
                keep_working_directory: false,
                environment: BTreeMap::new(),
            },
            update: UpdateSpec::default(),
            health: xpack_core::HealthSpec::default(),
            signing_key: None,
            desktop: xpack_core::DesktopSpec::default(),
            payload: PayloadSpec::default(),
            created_at: None,
            command: None,
            commands: Vec::new(),
            instance: InstanceSpec { single },
        };
        manifest.format_version = manifest.required_format_version();
        let package = dir.path().join("app.xpkg");
        PackageBuilder::new(&payload, manifest).build(&package, &key).unwrap();

        let paths = InstallPaths::new(&dir.path().join("apps"), "com.example.app").unwrap();
        let lock = InstallLock::acquire(&paths).unwrap();
        let mut verified =
            open_and_verify(&package, &lock, &TrustDecision::Explicit(key.public())).unwrap();
        Installer::new(&lock)
            .install(&mut verified, &InstallOptions { activate: true, ..Default::default() })
            .unwrap();
        drop(lock);

        // Where an install puts it: the launcher finds its installation from
        // the directory it sits in.
        let launcher = paths.root().join("xpack-launcher");
        fs::copy(env!("CARGO_BIN_EXE_xpack-launcher"), &launcher).unwrap();
        Self { _dir: dir, paths, launcher, running: Vec::new() }
    }

    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(&self.launcher);
        command
            .args(arguments)
            .env_remove("XPACK_BUNDLE")
            .env_remove("XPACK_RESTARTED")
            .env("XPACK_NO_UPDATE", "1")
            .stdin(Stdio::null());
        command
    }

    /// Starts the application and leaves it running, once it has started.
    fn start(&mut self, arguments: &[&str]) {
        let before = self.starts().len();
        let child = self.command(arguments).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        self.running.push(child.unwrap());
        self.wait_for_starts(before + 1);
    }

    /// Starts the application and waits for that start to finish.
    fn start_and_wait(&self, arguments: &[&str]) -> Output {
        let mut child =
            self.command(arguments).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("the start did not finish: it did not hand over");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        child.wait_with_output().unwrap()
    }

    fn starts(&self) -> Vec<String> {
        fs::read_to_string(self.paths.root().join("starts.log"))
            .unwrap_or_default()
            .lines()
            .map(ToString::to_string)
            .collect()
    }

    fn wait_for_starts(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.starts().len() < count {
            assert!(Instant::now() < deadline, "only {:?} after waiting", self.starts());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        let Ok(entries) = fs::read_dir(self.paths.instance_inbox_dir()) else {
            return Vec::new();
        };
        let mut files: Vec<_> = entries.map(|entry| entry.unwrap().path()).collect();
        files.sort();
        files.iter().map(|file| serde_json::from_slice(&fs::read(file).unwrap()).unwrap()).collect()
    }

    /// Kills the first launcher and the application it started, as a crash
    /// would: nothing gets to clean up.
    fn crash_first(&mut self) {
        let mut launcher = self.running.remove(0);
        let _ = Command::new("pkill").args(["-KILL", "-P", &launcher.id().to_string()]).status();
        let _ = launcher.kill();
        let _ = launcher.wait();
    }
}

impl Drop for World {
    fn drop(&mut self) {
        for mut child in self.running.drain(..) {
            let _ = Command::new("pkill").args(["-KILL", "-P", &child.id().to_string()]).status();
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn inbox_of(paths: &InstallPaths) -> String {
    paths.instance_inbox_dir().display().to_string()
}

#[test]
fn a_second_start_hands_its_arguments_to_the_running_copy_and_exits() {
    let mut world = World::new(true);
    world.start(&["first"]);

    let second = world.start_and_wait(&["--open", "a file.txt"]);

    assert!(second.status.success(), "{second:?}");
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("already running"),
        "a terminal start is told what happened: {second:?}"
    );
    assert_eq!(world.starts().len(), 1, "a second copy started: {:?}", world.starts());
    assert_eq!(
        world.requests(),
        [serde_json::json!({ "arguments": ["--open", "a file.txt"] })],
        "the running copy was not handed the arguments"
    );
    assert!(world.running[0].try_wait().unwrap().is_none(), "the running copy was disturbed");
}

#[test]
fn the_running_copy_is_told_where_requests_arrive() {
    let mut world = World::new(true);
    world.start(&[]);
    let inbox = inbox_of(&world.paths);
    assert_eq!(world.starts(), [format!("start |{inbox}")]);
}

#[test]
fn a_crashed_copy_does_not_keep_the_user_out() {
    let mut world = World::new(true);
    world.start(&["first"]);
    world.crash_first();

    world.start(&["again"]);

    assert_eq!(world.starts().len(), 2, "{:?}", world.starts());
    assert!(world.starts()[1].starts_with("start again|"), "{:?}", world.starts());
}

#[test]
fn requests_left_for_a_copy_that_has_gone_are_not_replayed_to_the_next() {
    let mut world = World::new(true);
    world.start(&[]);
    let _ = world.start_and_wait(&["for the first copy"]);
    assert_eq!(world.requests().len(), 1);
    world.crash_first();

    world.start(&[]);

    assert_eq!(world.requests(), Vec::<serde_json::Value>::new());
}

#[test]
fn an_application_that_does_not_ask_runs_as_many_copies_as_are_started() {
    let mut world = World::new(false);
    world.start(&["first"]);
    world.start(&["second"]);

    assert_eq!(world.starts().len(), 2, "{:?}", world.starts());
    assert!(world.starts()[1].starts_with("start second|"), "{:?}", world.starts());
    assert!(
        world.starts()[1].ends_with('|'),
        "told of an inbox it cannot use: {:?}",
        world.starts()
    );
    assert!(world.requests().is_empty());
}

#[test]
fn a_start_that_arrives_while_the_first_is_still_starting_is_not_lost() {
    // A first start takes the instance lock at once but may then wait for the
    // installation lock, which an updater can hold. Several files opened at
    // once start several launchers inside that wait.
    let mut world = World::new(true);

    // What a previous session left: a request for it, and a record naming
    // its application, whose process id may since belong to anything.
    fs::create_dir_all(world.paths.instance_inbox_dir()).unwrap();
    fs::write(
        world.paths.instance_inbox_dir().join("00000000000000000000000000000001-1.json"),
        r#"{"arguments":["from the last session"]}"#,
    )
    .unwrap();
    fs::write(world.paths.instance_record_file(), r#"{"pid":1}"#).unwrap();

    let installation = InstallLock::acquire(&world.paths).unwrap();
    let first = world.command(&["first"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    world.running.push(first.unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    // An error is "not yet" too: macOS briefly refuses the file while it
    // checks a newly copied executable on its first run.
    while !matches!(xpack_platform::InstanceLock::acquire(&world.paths), Ok(None)) {
        assert!(Instant::now() < deadline, "the first start never took the instance lock");
        std::thread::sleep(Duration::from_millis(20));
    }

    let second = world.start_and_wait(&["opened meanwhile"]);
    assert!(second.status.success(), "{second:?}");
    assert!(
        !world.paths.instance_record_file().exists(),
        "the last session's record was still there to be used"
    );

    drop(installation);
    world.wait_for_starts(1);

    assert_eq!(
        world.requests(),
        [serde_json::json!({ "arguments": ["opened meanwhile"] })],
        "a request was lost, or an old one kept"
    );
}
