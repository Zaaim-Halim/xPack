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
use xpack_core::state::UpdatePhase;
use xpack_core::{InstallPaths, InstallState, InstanceSpec, Manifest, Platform, Version};
use xpack_install::{InstallOptions, Installer, TrustDecision, open_and_verify};
use xpack_package::PackageBuilder;
use xpack_platform::InstallLock;
use xpack_security::KeyPair;

/// Appends one line per start, with the arguments and the inbox it was told
/// about, then keeps running until killed.
///
/// Two arguments make it a command instead: `--status` prints its version
/// and the inbox it was told about, and exits 3; `--hold` records itself and
/// keeps running, as a long command would.
const APP: &str = "#!/bin/sh\n\
    case \"$1\" in\n\
    --status*) printf 'status %s|%s\\n' \"$APP_VERSION\" \"$XPACK_INSTANCE_INBOX\"; exit 3 ;;\n\
    --hold) printf 'hold\\n' >> \"$XPACK_APPLICATION_DIR/commands.log\"; exec sleep 30 ;;\n\
    esac\n\
    printf 'start %s|%s\\n' \"$*\" \"$XPACK_INSTANCE_INBOX\" >> \"$XPACK_APPLICATION_DIR/starts.log\"\n\
    exec sleep 30\n";

struct World {
    dir: tempfile::TempDir,
    paths: InstallPaths,
    launcher: PathBuf,
    key: KeyPair,
    instance: InstanceSpec,
    running: Vec<Child>,
}

impl World {
    fn new(single: bool) -> Self {
        Self::with(InstanceSpec { single, alongside: Vec::new() })
    }

    /// A single-instance application whose package lets these arguments run
    /// beside the running copy.
    fn alongside(commands: &[&str]) -> Self {
        Self::with(InstanceSpec {
            single: true,
            alongside: commands.iter().map(ToString::to_string).collect(),
        })
    }

    fn with(instance: InstanceSpec) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let key = KeyPair::generate().unwrap();
        let paths = InstallPaths::new(&dir.path().join("apps"), "com.example.app").unwrap();
        let mut world = Self {
            launcher: paths.root().join("xpack-launcher"),
            dir,
            paths,
            key,
            instance,
            running: Vec::new(),
        };
        world.install("1.0.0", false, true);

        // Where an install puts it: the launcher finds its installation from
        // the directory it sits in.
        fs::copy(env!("CARGO_BIN_EXE_xpack-launcher"), &world.launcher).unwrap();
        world
    }

    /// Installs this version of the application; activated, or only staged
    /// as the updater leaves one.
    fn install(&mut self, version: &str, mandatory: bool, activate: bool) {
        let build = self.dir.path().join(format!("build-{version}"));
        let payload = build.join("payload");
        fs::create_dir_all(payload.join("bin")).unwrap();
        fs::write(payload.join("bin/app"), APP).unwrap();
        fs::set_permissions(payload.join("bin/app"), fs::Permissions::from_mode(0o755)).unwrap();

        let mut manifest = Manifest {
            format_version: FormatVersion::CURRENT,
            application: Application {
                id: "com.example.app".into(),
                name: "Example".into(),
                version: Version::parse(version).unwrap(),
                description: None,
                publisher: None,
            },
            platform: Platform::host().unwrap(),
            launch: LaunchSpec {
                executable: "bin/app".into(),
                arguments: vec![],
                working_directory: None,
                keep_working_directory: false,
                environment: BTreeMap::from([("APP_VERSION".into(), version.into())]),
            },
            update: UpdateSpec { mandatory, ..UpdateSpec::default() },
            health: xpack_core::HealthSpec::default(),
            signing_key: None,
            desktop: xpack_core::DesktopSpec::default(),
            payload: PayloadSpec::default(),
            created_at: None,
            command: None,
            commands: Vec::new(),
            instance: self.instance.clone(),
        };
        manifest.format_version = manifest.required_format_version();
        let package = build.join("app.xpkg");
        PackageBuilder::new(&payload, manifest).build(&package, &self.key).unwrap();

        let lock = InstallLock::acquire(&self.paths).unwrap();
        let mut verified =
            open_and_verify(&package, &lock, &TrustDecision::Explicit(self.key.public())).unwrap();
        Installer::new(&lock)
            .install(&mut verified, &InstallOptions { activate, ..Default::default() })
            .unwrap();
    }

    fn state(&self) -> InstallState {
        xpack_core::store::load::<InstallState>(&self.paths.state_file()).unwrap().value
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
    assert_eq!(world.requests(), Vec::<serde_json::Value>::new());
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

#[test]
fn a_listed_command_runs_beside_the_running_copy_and_answers_for_itself() {
    let mut world = World::alongside(&["--status"]);
    world.start(&["window"]);
    // The launcher records the copy just after starting it, so the
    // application can be up a moment before the record is.
    let deadline = Instant::now() + Duration::from_secs(10);
    let record = loop {
        match fs::read(world.paths.instance_record_file()) {
            Ok(record) => break record,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => panic!("the running copy was never recorded: {error}"),
        }
    };

    let status = world.start_and_wait(&["--status"]);

    // Its own output and exit code, not "already running" and success.
    assert_eq!(status.status.code(), Some(3), "{status:?}");
    assert_eq!(
        String::from_utf8_lossy(&status.stdout),
        "status 1.0.0|\n",
        "the command did not run, or was told where the running copy's requests arrive"
    );
    assert!(!String::from_utf8_lossy(&status.stderr).contains("already running"), "{status:?}");
    assert!(world.requests().is_empty(), "the command was handed over as well");
    assert_eq!(world.starts().len(), 1, "{:?}", world.starts());
    assert!(world.running[0].try_wait().unwrap().is_none(), "the running copy was disturbed");
    assert_eq!(
        fs::read(world.paths.instance_record_file()).unwrap(),
        record,
        "the command replaced the record of the running copy"
    );
}

#[test]
fn a_listed_command_with_a_value_runs_beside_and_anything_else_is_handed_over() {
    let mut world = World::alongside(&["--status"]);
    world.start(&[]);

    let with_value = world.start_and_wait(&["--status=short"]);
    assert_eq!(with_value.status.code(), Some(3), "{with_value:?}");

    // Not the listed argument, only one that begins the same way.
    let other = world.start_and_wait(&["--status-bar"]);
    assert!(other.status.success(), "{other:?}");
    assert!(String::from_utf8_lossy(&other.stderr).contains("already running"), "{other:?}");
    assert_eq!(world.requests(), [serde_json::json!({ "arguments": ["--status-bar"] })]);
    assert_eq!(world.starts().len(), 1, "{:?}", world.starts());
}

#[test]
fn a_command_running_with_no_copy_open_does_not_become_the_running_copy() {
    // Were it to take the instance lock, the window opened while it runs
    // would be handed over to a command, and never appear.
    let mut world = World::alongside(&["--hold"]);
    let command = world.command(&["--hold"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    world.running.push(command.unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !world.paths.root().join("commands.log").exists() {
        assert!(Instant::now() < deadline, "the command never ran");
        std::thread::sleep(Duration::from_millis(20));
    }

    world.start(&["window"]);

    assert!(world.starts()[0].starts_with("start window|"), "{:?}", world.starts());
    assert_eq!(world.requests(), Vec::<serde_json::Value>::new());
}

#[test]
fn a_command_neither_activates_a_staged_version_nor_ends_its_wait() {
    let mut world = World::alongside(&["--status"]);
    world.start(&[]);
    world.install("1.1.0", false, false);
    let before = world.state();
    assert!(matches!(before.update, UpdatePhase::Staged { .. }), "{:?}", before.update);

    let status = world.start_and_wait(&["--status"]);

    // The version the running copy runs, which a start handing over to it
    // reads, stays the active one; the staged one waits for the next start
    // of the application, which watches it start.
    assert_eq!(String::from_utf8_lossy(&status.stdout), "status 1.0.0|\n", "{status:?}");
    let after = world.state();
    assert_eq!(after.active().unwrap().to_string(), "1.0.0");
    assert_eq!(after.update, before.update);
}

#[test]
fn a_command_does_not_run_a_version_a_mandatory_release_has_retired() {
    let mut world = World::alongside(&["--status"]);
    world.start(&[]);
    world.install("1.1.0", true, false);

    let status = world.start_and_wait(&["--status"]);

    assert!(!status.status.success(), "{status:?}");
    assert!(status.stdout.is_empty(), "the retired version ran: {status:?}");
    let said = String::from_utf8_lossy(&status.stderr);
    assert!(said.contains("1.1.0 is required and is ready to use"), "{said}");
    assert!(said.contains("start it"), "{said}");
    assert_eq!(world.state().active().unwrap().to_string(), "1.0.0");
}

#[test]
fn a_listed_command_typed_through_the_installed_command_runs_beside_the_running_copy() {
    // What a user types: the script an install puts on the PATH, which names
    // the command and passes the arguments on.
    let mut world = World::alongside(&["--status"]);
    world.start(&[]);
    let script = world.dir.path().join("example");
    fs::write(
        &script,
        xpack_install::integration::command::script(
            &xpack_install::integration::command::Command {
                application_id: "com.example.app".into(),
                name: "example".into(),
                launcher: world.launcher.clone(),
                command_dir: world.paths.root().join(xpack_core::paths::COMMAND_DIR),
            },
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let status = Command::new(&script)
        .arg("--status")
        .env_remove("XPACK_BUNDLE")
        .env_remove("XPACK_RESTARTED")
        .env("XPACK_NO_UPDATE", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert_eq!(status.status.code(), Some(3), "{status:?}");
    assert_eq!(String::from_utf8_lossy(&status.stdout), "status 1.0.0|\n", "{status:?}");
    assert!(world.requests().is_empty(), "the command was handed over");
    assert_eq!(world.starts().len(), 1, "{:?}", world.starts());
}

/// Whether an installer could take the presence lock alone right now, as it
/// must before replacing the installation's programs.
fn an_installer_could_replace(paths: &InstallPaths) -> bool {
    xpack_platform::PresenceLock::exclusive(paths).unwrap().is_some()
}

/// Waits until `condition` holds, failing with `what` after ten seconds.
fn eventually(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_running_application_keeps_an_installer_from_replacing_its_programs() {
    let mut world = World::new(false);
    assert!(an_installer_could_replace(&world.paths), "busy before anything ran");

    world.start(&["first"]);
    assert!(!an_installer_could_replace(&world.paths), "an installer was let in while it ran");

    world.crash_first();
    eventually("an ended launcher kept an installer out", || {
        an_installer_could_replace(&world.paths)
    });
}

#[test]
fn a_second_copy_keeps_an_installer_out_after_the_first_has_gone() {
    // The instance lock goes with the first copy; a second copy of an
    // application that allows several still runs the same programs.
    let mut world = World::new(false);
    world.start(&["first"]);
    world.start(&["second"]);
    world.crash_first();
    assert!(
        xpack_platform::InstanceLock::acquire(&world.paths).unwrap().is_some(),
        "the instance lock was still held, so this proves nothing"
    );

    assert!(
        !an_installer_could_replace(&world.paths),
        "an installer was let in while the second copy ran"
    );
}

#[test]
fn a_command_running_beside_the_window_keeps_an_installer_out() {
    let mut world = World::alongside(&["--hold"]);
    let command = world.command(&["--hold"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    world.running.push(command.unwrap());
    eventually("the command never ran", || world.paths.root().join("commands.log").exists());

    assert!(!an_installer_could_replace(&world.paths), "an installer was let in during a command");
}

#[test]
fn a_start_waits_while_an_installer_replaces_the_programs() {
    let mut world = World::new(false);
    let installer = xpack_platform::PresenceLock::exclusive(&world.paths).unwrap().unwrap();

    let start = world.command(&["waited"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    world.running.push(start.unwrap());
    std::thread::sleep(Duration::from_secs(1));
    assert!(world.starts().is_empty(), "it started in the middle of a replacement");

    drop(installer);
    world.wait_for_starts(1);
    assert!(world.starts()[0].starts_with("start waited|"), "{:?}", world.starts());
}

#[test]
fn a_start_that_meets_a_replacement_still_going_says_so_and_starts_nothing() {
    let world = World::new(false);
    let _installer = xpack_platform::PresenceLock::exclusive(&world.paths).unwrap().unwrap();

    let mut start =
        world.command(&[]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    // It waits fifteen seconds for the installer before giving up.
    let deadline = Instant::now() + Duration::from_secs(40);
    while start.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = start.kill();
            panic!("the start never gave up waiting");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let output = start.wait_with_output().unwrap();

    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("being installed or updated"),
        "{output:?}"
    );
    assert!(world.starts().is_empty(), "it started anyway: {:?}", world.starts());
}

#[test]
fn a_start_that_finds_a_replacement_it_cannot_resolve_starts_nothing() {
    // A replacement of the programs stopped part way, and its record cannot
    // be read: running whatever mixture is on disk is the one thing not done.
    let world = World::new(false);
    fs::write(world.paths.runtime_replacement_journal_file(), b"{ torn").unwrap();

    let output = world.start_and_wait(&[]);

    assert!(!output.status.success(), "{output:?}");
    assert!(world.starts().is_empty(), "it started anyway: {:?}", world.starts());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("run the installer again"),
        "{output:?}"
    );
}
