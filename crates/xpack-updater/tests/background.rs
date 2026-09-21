//! The background updater: when it asks, and what it does with the answer.
//!
//! The scaffolding mirrors the update engine's own tests. It is duplicated
//! rather than shared because Rust integration tests cannot import each
//! other's helpers, and a crate existing only to hold them would be a worse
//! trade than this.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use xpack_core::manifest::{Application, FormatVersion, LaunchSpec, PayloadSpec, UpdateSpec};
use xpack_core::{Error, InstallPaths, Manifest, Platform, Result, Version};
use xpack_install::{InstallOptions, Installer, TrustDecision, open_and_verify};
use xpack_package::PackageBuilder;
use xpack_platform::InstallLock;
use xpack_security::KeyPair;
use xpack_update::UpdateTransport;

/// A transport that serves whatever a test puts in it.
#[derive(Default)]
struct Fixture {
    routes: Mutex<BTreeMap<String, Vec<u8>>>,
    /// How many fetches were attempted, so a test can assert the server was
    /// never contacted at all rather than merely that nothing was installed.
    requests: Mutex<usize>,
}

impl Fixture {
    fn requests(&self) -> usize {
        *self.requests.lock().unwrap()
    }

    fn serve(&self, url: &str, bytes: Vec<u8>) {
        self.routes.lock().unwrap().insert(url.to_string(), bytes);
    }

    fn serve_file(&self, url: &str, path: &Path) {
        self.serve(url, std::fs::read(path).unwrap());
    }
}

impl UpdateTransport for Fixture {
    fn fetch(&self, url: &str, limit: u64, sink: &mut dyn Write) -> Result<u64> {
        *self.requests.lock().unwrap() += 1;
        xpack_update::transport::ensure_secure(url)?;

        let routes = self.routes.lock().unwrap();
        let body =
            routes.get(url).ok_or_else(|| Error::Transport(format!("{url} returned HTTP 404")))?;
        let mut reader = std::io::Cursor::new(body.clone());
        xpack_update::transport::stream_bounded(&mut reader, sink, limit, url)
    }
}

const BASE: &str = "https://updates.example.com/demo";

fn index_url() -> String {
    format!("{BASE}/stable.json")
}

fn build_package(dir: &Path, key: &KeyPair, version: &str) -> PathBuf {
    let payload = dir.join(format!("src-{version}"));
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/app"), format!("#!/bin/sh\necho {version}\n")).unwrap();

    let manifest = Manifest {
        format_version: FormatVersion::CURRENT,
        application: Application {
            id: "com.example.demo".into(),
            name: "Demo".into(),
            version: Version::parse(version).unwrap(),
            description: None,
            publisher: None,
        },
        platform: Platform::host().unwrap(),
        launch: LaunchSpec {
            executable: "bin/app".into(),
            arguments: vec![],
            working_directory: None,
            environment: BTreeMap::new(),
        },
        update: UpdateSpec { url: Some(BASE.to_string()), ..UpdateSpec::default() },
        health: xpack_core::HealthSpec::default(),
        signing_key: None,
        desktop: xpack_core::DesktopSpec::default(),
        payload: PayloadSpec::default(),
        created_at: None,
    };
    let out = dir.join(format!("demo-{version}.xpkg"));
    PackageBuilder::new(&payload, manifest).build(&out, key).unwrap();
    out
}

fn index_json(version: &str, file: &str, size: u64) -> Vec<u8> {
    format!(
        r#"{{"application":"com.example.demo","version":"{version}",
            "platform":{{"os":"{}","arch":"{}"}},"channel":"stable",
            "package":{{"file":"{file}","size":{size}}}}}"#,
        Platform::host().unwrap().os,
        Platform::host().unwrap().arch
    )
    .into_bytes()
}

/// An installation already running 1.0.0.
struct World {
    dir: tempfile::TempDir,
    key: KeyPair,
    paths: InstallPaths,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let key = KeyPair::generate().unwrap();
        let paths = InstallPaths::new(dir.path(), "com.example.demo").unwrap();

        let package = build_package(dir.path(), &key, "1.0.0");
        let lock = InstallLock::acquire(&paths).unwrap();
        let mut verified =
            open_and_verify(&package, &lock, &TrustDecision::Explicit(key.public())).unwrap();
        Installer::new(&lock)
            .install(
                &mut verified,
                &InstallOptions { activate: true, allow_downgrade: false, ..Default::default() },
            )
            .unwrap();

        Self { dir, key, paths }
    }

    fn lock(&self) -> InstallLock {
        InstallLock::acquire(&self.paths).unwrap()
    }

    fn active(&self) -> Option<Version> {
        let lock = self.lock();
        lock.load_state().unwrap().value.current_version
    }
}

/// Records a check at `when`, as a previous run would have.
fn record_check_at(world: &World, when: u64) {
    let lock = world.lock();
    let mut state = lock.load_state().unwrap().value;
    state.last_update_check = Some(when);
    lock.save_state(&state).unwrap();
}

/// Reads the recorded last-check instant.
fn last_check(world: &World) -> Option<u64> {
    world.lock().load_state().unwrap().value.last_update_check
}

/// The update phase this installation is in.
fn phase(world: &World) -> xpack_core::state::UpdatePhase {
    world.lock().load_state().unwrap().value.update
}

use std::time::Duration;
use xpack_updater::{BackgroundUpdater, Outcome, unix_seconds};

/// Serves a genuine 1.1.0 package.
fn serving_an_update(world: &World) -> Fixture {
    let package = build_package(world.dir.path(), &world.key, "1.1.0");
    let size = std::fs::metadata(&package).unwrap().len();
    let fixture = Fixture::default();
    fixture.serve(&index_url(), index_json("1.1.0", "demo-1.1.0.xpkg", size));
    fixture.serve_file(&format!("{BASE}/demo-1.1.0.xpkg"), &package);
    fixture
}

#[test]
fn a_due_check_stages_a_new_version_without_activating_it() {
    // The whole point of the background updater: the new version is on disk
    // and verified, and the running application is untouched.
    let world = World::new();
    let fixture = serving_an_update(&world);

    let outcome = BackgroundUpdater::new(&world.paths, &fixture).run().unwrap();

    assert_eq!(outcome, Outcome::Staged(Version::parse("1.1.0").unwrap()));
    assert_eq!(
        world.active(),
        Some(Version::parse("1.0.0").unwrap()),
        "the background updater activated a version; only the launcher may do that"
    );
    assert!(
        world.paths.version_dir(&Version::parse("1.1.0").unwrap()).is_dir(),
        "the staged version is not on disk"
    );
    assert!(
        matches!(phase(&world), xpack_core::state::UpdatePhase::Staged { ref version }
            if version == &Version::parse("1.1.0").unwrap()),
        "got {:?}",
        phase(&world)
    );
}

#[test]
fn a_check_inside_the_interval_never_reaches_the_server() {
    // An application opened twenty times a day must not mean twenty requests.
    let world = World::new();
    let fixture = serving_an_update(&world);
    record_check_at(&world, unix_seconds());

    let outcome = BackgroundUpdater::new(&world.paths, &fixture).run().unwrap();

    assert_eq!(outcome, Outcome::NotDue);
    assert_eq!(fixture.requests(), 0, "the server was contacted when no check was due");
    assert!(!world.paths.version_dir(&Version::parse("1.1.0").unwrap()).exists());
}

#[test]
fn an_elapsed_interval_makes_a_check_due_again() {
    let world = World::new();
    let fixture = serving_an_update(&world);
    record_check_at(&world, unix_seconds() - 5 * 60 * 60);

    let outcome = BackgroundUpdater::new(&world.paths, &fixture)
        .every(Duration::from_secs(4 * 60 * 60))
        .run()
        .unwrap();

    assert_eq!(outcome, Outcome::Staged(Version::parse("1.1.0").unwrap()));
}

#[test]
fn forcing_overrides_the_interval() {
    let world = World::new();
    let fixture = serving_an_update(&world);
    record_check_at(&world, unix_seconds());

    let outcome = BackgroundUpdater::new(&world.paths, &fixture).forced(true).run().unwrap();

    assert_eq!(outcome, Outcome::Staged(Version::parse("1.1.0").unwrap()));
}

#[test]
fn the_check_is_recorded_even_when_the_server_fails() {
    // Recording only on success would let a server that always fails be
    // retried on every single application start.
    let world = World::new();
    let fixture = Fixture::default(); // serves nothing; every fetch is a 404

    let before = last_check(&world);
    assert_eq!(before, None);

    let _ = BackgroundUpdater::new(&world.paths, &fixture).run();

    assert!(last_check(&world).is_some(), "a failed check was not recorded");
}

#[test]
fn an_installation_with_no_update_server_asks_nobody() {
    let world = World::new();
    let fixture = serving_an_update(&world);

    // Strip the update URL the manifest carries.
    let manifest_file = world.paths.version_manifest_file(&Version::parse("1.0.0").unwrap());
    let manifest = Manifest::from_slice(&std::fs::read(&manifest_file).unwrap()).unwrap();
    let mut stripped = manifest.clone();
    stripped.update.url = None;
    std::fs::write(&manifest_file, serde_json::to_vec(&stripped).unwrap()).unwrap();

    let outcome = BackgroundUpdater::new(&world.paths, &fixture).run().unwrap();

    assert_eq!(outcome, Outcome::NoServerConfigured);
    assert_eq!(fixture.requests(), 0);
}

/// Rewrites the installed manifest's declared check interval.
///
/// The same trick the no-server test uses: the installed copy is what the
/// updater reads, so a test changes what the publisher declared by changing
/// it there.
fn declare_check_interval(world: &World, minutes: Option<u32>) {
    let manifest_file = world.paths.version_manifest_file(&Version::parse("1.0.0").unwrap());
    let mut manifest = Manifest::from_slice(&std::fs::read(&manifest_file).unwrap()).unwrap();
    manifest.update.check_interval_minutes = minutes;
    std::fs::write(&manifest_file, serde_json::to_vec(&manifest).unwrap()).unwrap();
}

#[test]
fn a_manifest_interval_shorter_than_the_default_makes_a_check_due_sooner() {
    // Without this the launcher would spawn the updater every thirty minutes,
    // as the publisher asked, and be told each time that nothing is due for
    // another four hours.
    let world = World::new();
    let fixture = serving_an_update(&world);
    declare_check_interval(&world, Some(30));
    record_check_at(&world, unix_seconds() - 60 * 60);

    let outcome = BackgroundUpdater::new(&world.paths, &fixture).run().unwrap();

    assert_eq!(outcome, Outcome::Staged(Version::parse("1.1.0").unwrap()));
}

#[test]
fn a_manifest_interval_longer_than_the_default_holds_a_check_back() {
    let world = World::new();
    let fixture = serving_an_update(&world);
    declare_check_interval(&world, Some(24 * 60));
    record_check_at(&world, unix_seconds() - 5 * 60 * 60);

    let outcome = BackgroundUpdater::new(&world.paths, &fixture).run().unwrap();

    assert_eq!(outcome, Outcome::NotDue, "the default interval overrode the publisher's");
    assert_eq!(fixture.requests(), 0);
}

#[test]
fn an_explicit_interval_overrides_the_manifest() {
    // An operator running the binary by hand has a reason the manifest cannot
    // know about.
    let world = World::new();
    let fixture = serving_an_update(&world);
    declare_check_interval(&world, Some(24 * 60));
    record_check_at(&world, unix_seconds() - 2 * 60 * 60);

    let outcome = BackgroundUpdater::new(&world.paths, &fixture)
        .every(Duration::from_secs(60 * 60))
        .run()
        .unwrap();

    assert_eq!(outcome, Outcome::Staged(Version::parse("1.1.0").unwrap()));
}

#[test]
fn a_manifest_declaring_no_interval_asks_at_the_default_rate() {
    let world = World::new();
    let fixture = serving_an_update(&world);
    declare_check_interval(&world, None);
    record_check_at(&world, unix_seconds() - 2 * 60 * 60);

    let outcome = BackgroundUpdater::new(&world.paths, &fixture).run().unwrap();

    assert_eq!(outcome, Outcome::NotDue);
    assert_eq!(fixture.requests(), 0);
}

#[test]
fn a_manifest_interval_below_the_floor_does_not_mean_a_check_every_start() {
    // A publisher asking for a check every minute gets the floor, not a
    // request from every installation every minute.
    let world = World::new();
    let fixture = serving_an_update(&world);
    declare_check_interval(&world, Some(1));
    record_check_at(&world, unix_seconds() - 5 * 60);

    let outcome = BackgroundUpdater::new(&world.paths, &fixture).run().unwrap();

    assert_eq!(outcome, Outcome::NotDue);
    assert_eq!(fixture.requests(), 0);
}

/// Declares whether the installed version checks while it runs.
fn declare_checking_while_running(world: &World, checks: bool) {
    let manifest_file = world.paths.version_manifest_file(&Version::parse("1.0.0").unwrap());
    let mut manifest = Manifest::from_slice(&std::fs::read(&manifest_file).unwrap()).unwrap();
    manifest.update.check_while_running = checks;
    std::fs::write(&manifest_file, serde_json::to_vec(&manifest).unwrap()).unwrap();
}

#[test]
fn the_interval_governs_a_startup_check_too() {
    // The interval says how often the server may be asked, and a check made
    // when the application starts is asking. An installation that never
    // checks while running still honours it.
    let world = World::new();
    let fixture = serving_an_update(&world);
    declare_checking_while_running(&world, false);
    declare_check_interval(&world, Some(24 * 60));
    record_check_at(&world, unix_seconds() - 5 * 60 * 60);

    let outcome = BackgroundUpdater::new(&world.paths, &fixture).run().unwrap();

    assert_eq!(outcome, Outcome::NotDue, "a startup check ignored the declared interval");
    assert_eq!(fixture.requests(), 0);
}

#[test]
fn forgetting_the_interval_does_not_stop_an_installation_checking() {
    // The failure the switch exists to prevent. When the interval was also
    // the switch, a release that left the number out stopped its
    // installations checking at all, silently. Now it checks at the default
    // rate, and only the switch can turn it off.
    let world = World::new();
    let fixture = serving_an_update(&world);
    declare_checking_while_running(&world, true);
    declare_check_interval(&world, None);
    record_check_at(&world, unix_seconds() - 5 * 60 * 60);

    let outcome = BackgroundUpdater::new(&world.paths, &fixture).run().unwrap();

    assert_eq!(outcome, Outcome::Staged(Version::parse("1.1.0").unwrap()));
}
