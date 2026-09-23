//! The update engine against a server that lies.
//!
//! Every test here is a server behaving badly. A passing run means the
//! installation stayed on the version it already had.

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
use xpack_update::{UpdateOptions, UpdateTransport, Updater};

/// A transport that serves whatever a test puts in it.
#[derive(Default)]
struct Fixture {
    routes: Mutex<BTreeMap<String, Vec<u8>>>,
    /// A URL that overruns, and by how much.
    ///
    /// Scoped to one URL on purpose. Flooding every fetch would trip on the
    /// index and never reach the package download, so the test would pass
    /// without exercising the path it names.
    flood: Mutex<Option<(String, u64)>>,
    /// Every URL fetched, in order.
    ///
    /// Lets a test assert on what was *not* requested, which is the only way
    /// to show that a decision was taken before the transfer rather than after.
    fetched: Mutex<Vec<String>>,
}

impl Fixture {
    fn serve(&self, url: &str, bytes: Vec<u8>) {
        self.routes.lock().unwrap().insert(url.to_string(), bytes);
    }

    fn serve_file(&self, url: &str, path: &Path) {
        self.serve(url, std::fs::read(path).unwrap());
    }

    fn flood(&self, url: &str, bytes: u64) {
        *self.flood.lock().unwrap() = Some((url.to_string(), bytes));
    }

    fn was_fetched(&self, url: &str) -> bool {
        self.fetched.lock().unwrap().iter().any(|seen| seen == url)
    }
}

impl UpdateTransport for Fixture {
    fn fetch(&self, url: &str, limit: u64, sink: &mut dyn Write) -> Result<u64> {
        xpack_update::transport::ensure_secure(url)?;
        self.fetched.lock().unwrap().push(url.to_string());

        let flood = self.flood.lock().unwrap().clone();
        if let Some((flooded, total)) = flood
            && flooded == url
        {
            // Streams far more than allowed, one chunk at a time, exactly as a
            // hostile server would.
            let chunk = vec![0u8; 8192];
            let mut reader = std::io::Cursor::new(chunk.repeat((total / 8192) as usize + 1));
            return xpack_update::transport::stream_bounded(&mut reader, sink, limit, url);
        }

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
    std::fs::create_dir_all(payload.join("runtime")).unwrap();
    std::fs::write(payload.join("bin/app"), format!("#!/bin/sh\necho {version}\n")).unwrap();
    // Byte-identical across versions, standing in for a bundled runtime. A
    // payload where every file changes would let a delta carry all of it and
    // never read the installed version, which is exactly the path these tests
    // exist to exercise.
    std::fs::write(payload.join("runtime/lib.bin"), b"a runtime that never changes").unwrap();

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
            keep_working_directory: false,
            environment: BTreeMap::new(),
        },
        update: UpdateSpec::default(),
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

fn options() -> UpdateOptions {
    UpdateOptions { activate: true, allow_downgrade: false, ..Default::default() }
}

#[test]
fn a_genuine_update_is_downloaded_verified_and_installed() {
    let world = World::new();
    let package = build_package(world.dir.path(), &world.key, "1.1.0");
    let size = std::fs::metadata(&package).unwrap().len();

    let fixture = Fixture::default();
    fixture.serve(&index_url(), index_json("1.1.0", "demo-1.1.0.xpkg", size));
    fixture.serve_file(&format!("{BASE}/demo-1.1.0.xpkg"), &package);

    let installed = Updater::new(&world.paths, &fixture).update(BASE, &options()).unwrap();

    assert_eq!(installed, Some(Version::parse("1.1.0").unwrap()));
    assert_eq!(world.active(), Some(Version::parse("1.1.0").unwrap()));
}

#[test]
fn a_package_signed_by_another_key_is_refused() {
    // The index is honest; the package is not. This is the case that matters
    // most: a compromised server serving a package it signed itself.
    let world = World::new();
    let attacker = KeyPair::generate().unwrap();
    let forged = build_package(world.dir.path(), &attacker, "1.1.0");
    let size = std::fs::metadata(&forged).unwrap().len();

    let fixture = Fixture::default();
    fixture.serve(&index_url(), index_json("1.1.0", "demo-1.1.0.xpkg", size));
    fixture.serve_file(&format!("{BASE}/demo-1.1.0.xpkg"), &forged);

    let err = Updater::new(&world.paths, &fixture).update(BASE, &options()).unwrap_err();

    assert!(err.is_integrity_failure(), "got {err:?}");
    assert_eq!(world.active(), Some(Version::parse("1.0.0").unwrap()), "must stay put");
}

#[test]
fn an_index_that_lies_about_the_version_is_caught() {
    // The index offers 2.0.0 but serves a genuinely signed 1.1.0. The
    // signature verifies, so only comparing the index against the manifest
    // catches this.
    let world = World::new();
    let package = build_package(world.dir.path(), &world.key, "1.1.0");
    let size = std::fs::metadata(&package).unwrap().len();

    let fixture = Fixture::default();
    fixture.serve(&index_url(), index_json("2.0.0", "demo-1.1.0.xpkg", size));
    fixture.serve_file(&format!("{BASE}/demo-1.1.0.xpkg"), &package);

    let err = Updater::new(&world.paths, &fixture).update(BASE, &options()).unwrap_err();

    assert!(err.to_string().contains("offered 2.0.0"), "got {err}");
    assert_eq!(world.active(), Some(Version::parse("1.0.0").unwrap()));
}

#[test]
fn a_server_that_streams_forever_is_cut_off() {
    let world = World::new();
    let fixture = Fixture::default();
    fixture.serve(&index_url(), index_json("1.1.0", "demo-1.1.0.xpkg", 1024));
    // Only the package floods, so the index is served normally and the
    // download path is the one actually under test.
    fixture.flood(&format!("{BASE}/demo-1.1.0.xpkg"), 16 * 1024 * 1024);

    let err = Updater::new(&world.paths, &fixture).update(BASE, &options()).unwrap_err();

    assert!(err.to_string().contains("limit"), "got {err}");
    assert_eq!(world.active(), Some(Version::parse("1.0.0").unwrap()));

    // Nothing oversized may be left on disk.
    let leftovers: Vec<_> = std::fs::read_dir(world.paths.downloads_dir())
        .map(|d| d.filter_map(std::result::Result::ok).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "a failed download must leave nothing behind");
}

#[test]
fn a_truncated_download_is_refused_and_discarded() {
    let world = World::new();
    let package = build_package(world.dir.path(), &world.key, "1.1.0");
    let mut bytes = std::fs::read(&package).unwrap();
    let full = bytes.len() as u64;
    bytes.truncate(bytes.len() / 2);

    let fixture = Fixture::default();
    fixture.serve(&index_url(), index_json("1.1.0", "demo-1.1.0.xpkg", full));
    fixture.serve(&format!("{BASE}/demo-1.1.0.xpkg"), bytes);

    assert!(Updater::new(&world.paths, &fixture).update(BASE, &options()).is_err());
    assert_eq!(world.active(), Some(Version::parse("1.0.0").unwrap()));
}

#[test]
fn an_index_offering_an_older_version_is_ignored() {
    // The downgrade attack: a valid, correctly signed older package with a
    // known vulnerability.
    let world = World::new();
    let older = build_package(world.dir.path(), &world.key, "0.9.0");
    let size = std::fs::metadata(&older).unwrap().len();

    let fixture = Fixture::default();
    fixture.serve(&index_url(), index_json("0.9.0", "demo-0.9.0.xpkg", size));
    fixture.serve_file(&format!("{BASE}/demo-0.9.0.xpkg"), &older);

    let updater = Updater::new(&world.paths, &fixture);
    assert!(updater.check(BASE, &options()).unwrap().is_none());
    assert_eq!(updater.update(BASE, &options()).unwrap(), None);
    assert_eq!(world.active(), Some(Version::parse("1.0.0").unwrap()));
}

#[test]
fn an_index_for_a_different_application_is_refused() {
    let world = World::new();
    let fixture = Fixture::default();
    let index = String::from_utf8(index_json("1.1.0", "x.xpkg", 10))
        .unwrap()
        .replace("com.example.demo", "com.attacker.app");
    fixture.serve(&index_url(), index.into_bytes());

    let err = Updater::new(&world.paths, &fixture).update(BASE, &options()).unwrap_err();
    assert!(err.to_string().contains("com.attacker.app"), "got {err}");
}

#[test]
fn an_index_for_another_platform_is_refused() {
    let world = World::new();
    let host = Platform::host().unwrap();
    let other = if host.os == xpack_core::platform::Os::Linux { "windows" } else { "linux" };
    let index = String::from_utf8(index_json("1.1.0", "x.xpkg", 10))
        .unwrap()
        .replace(&format!("\"{}\"", host.os), &format!("\"{other}\""));

    let fixture = Fixture::default();
    fixture.serve(&index_url(), index.into_bytes());

    let err = Updater::new(&world.paths, &fixture).update(BASE, &options()).unwrap_err();
    assert!(matches!(err, Error::PlatformMismatch { .. }), "got {err:?}");
}

#[test]
fn a_plain_http_url_is_refused() {
    let world = World::new();
    let fixture = Fixture::default();
    let err = Updater::new(&world.paths, &fixture)
        .update("http://updates.example.com/demo", &options())
        .unwrap_err();
    assert!(err.to_string().contains("https"), "got {err}");
}

#[test]
fn the_download_name_comes_from_the_installation_not_the_index() {
    // package.file is attacker-controlled; using it as a local path would be
    // the same hole as trusting an archive entry name.
    let world = World::new();
    let package = build_package(world.dir.path(), &world.key, "1.1.0");
    let size = std::fs::metadata(&package).unwrap().len();

    let fixture = Fixture::default();
    let hostile_name = "../../../../tmp/xpack-escape.xpkg";
    fixture.serve(&index_url(), index_json("1.1.0", hostile_name, size));
    fixture.serve_file(&format!("{BASE}/{hostile_name}"), &package);

    let installed = Updater::new(&world.paths, &fixture).update(BASE, &options()).unwrap();

    assert_eq!(installed, Some(Version::parse("1.1.0").unwrap()));
    assert!(
        !Path::new("/tmp/xpack-escape.xpkg").exists(),
        "the index must never choose a local path"
    );
}

#[test]
fn an_oversized_index_is_refused_before_parsing() {
    let world = World::new();
    let fixture = Fixture::default();
    fixture.serve(&index_url(), vec![b'{'; 2 * 1024 * 1024]);

    let err = Updater::new(&world.paths, &fixture).update(BASE, &options()).unwrap_err();
    assert!(err.to_string().contains("limit"), "got {err}");
}

#[test]
fn a_failed_update_leaves_the_phase_idle() {
    // A stuck phase would make every later command think an operation is in
    // flight and try to recover from it.
    let world = World::new();
    let attacker = KeyPair::generate().unwrap();
    let forged = build_package(world.dir.path(), &attacker, "1.1.0");
    let size = std::fs::metadata(&forged).unwrap().len();

    let fixture = Fixture::default();
    fixture.serve(&index_url(), index_json("1.1.0", "demo-1.1.0.xpkg", size));
    fixture.serve_file(&format!("{BASE}/demo-1.1.0.xpkg"), &forged);

    assert!(Updater::new(&world.paths, &fixture).update(BASE, &options()).is_err());
    let state = world.lock().load_state().unwrap().value;

    assert!(state.update.is_idle(), "got {:?}", state.update);
}

#[test]
fn check_reports_an_available_update_without_downloading() {
    let world = World::new();
    let fixture = Fixture::default();
    fixture.serve(&index_url(), index_json("1.1.0", "demo-1.1.0.xpkg", 4096));
    // The package is deliberately not served: check must not fetch it.

    let available = Updater::new(&world.paths, &fixture).check(BASE, &options()).unwrap();

    let available = available.expect("an update is available");
    assert_eq!(available.version, Version::parse("1.1.0").unwrap());
    assert_eq!(available.declared_size, 4096);
}

#[test]
fn the_downloaded_file_is_not_deleted_while_it_is_still_in_use() {
    // The update engine sets the Verifying phase, and the installer recovers
    // at the start of every operation. Recovery's rule for Verifying is to
    // clear the downloads directory — which would destroy the file the engine
    // is reading from.
    //
    // On Unix an unlinked file stays readable through its open handle, so this
    // survives by luck. On Windows deleting an open file fails outright, which
    // would make every update fail. The phase must therefore be resolved
    // before the installer runs, not left for it to clean up.
    let world = World::new();
    let package = build_package(world.dir.path(), &world.key, "1.1.0");
    let size = std::fs::metadata(&package).unwrap().len();

    let fixture = Fixture::default();
    fixture.serve(&index_url(), index_json("1.1.0", "demo-1.1.0.xpkg", size));
    fixture.serve_file(&format!("{BASE}/demo-1.1.0.xpkg"), &package);

    let installed = Updater::new(&world.paths, &fixture).update(BASE, &options()).unwrap();
    assert_eq!(installed, Some(Version::parse("1.1.0").unwrap()));

    // The downloads directory must still exist: recovery clearing it mid-
    // operation is what this guards against.
    assert!(
        world.paths.downloads_dir().exists(),
        "the downloads directory was destroyed during the install"
    );
}

/// Records every event an update emits.
#[derive(Default)]
struct Recorder(Mutex<Vec<xpack_core::progress::ProgressEvent>>);

impl xpack_core::progress::ProgressReporter for Recorder {
    fn report(&self, event: &xpack_core::progress::ProgressEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

impl Recorder {
    fn events(&self) -> Vec<xpack_core::progress::ProgressEvent> {
        self.0.lock().unwrap().clone()
    }

    fn has(&self, predicate: impl Fn(&xpack_core::progress::ProgressEvent) -> bool) -> bool {
        self.events().iter().any(predicate)
    }
}

#[test]
fn an_update_reports_every_stage_it_passes_through() {
    use xpack_core::progress::ProgressEvent as E;

    // A consumer driving a splash screen has no way to poll a function that
    // has not returned, so the events are the only thing it can react to.
    let world = World::new();
    let package = build_package(world.dir.path(), &world.key, "1.1.0");
    let size = std::fs::metadata(&package).unwrap().len();

    let fixture = Fixture::default();
    fixture.serve(&index_url(), index_json("1.1.0", "demo-1.1.0.xpkg", size));
    fixture.serve_file(&format!("{BASE}/demo-1.1.0.xpkg"), &package);

    let recorder = Recorder::default();
    let installed = Updater::new(&world.paths, &fixture)
        .reporting_to(&recorder)
        .update(BASE, &options())
        .unwrap();
    assert_eq!(installed, Some(Version::parse("1.1.0").unwrap()));

    let events = recorder.events();
    assert!(!events.is_empty(), "an update must report something");

    assert!(recorder.has(|e| matches!(e, E::DownloadStarted { .. })), "{events:?}");
    assert!(recorder.has(|e| matches!(e, E::DownloadProgress { .. })), "{events:?}");
    assert!(recorder.has(|e| matches!(e, E::DownloadCompleted { .. })), "{events:?}");
    assert!(recorder.has(|e| matches!(e, E::Verifying { .. })), "{events:?}");
    assert!(recorder.has(|e| matches!(e, E::Installing { .. })), "{events:?}");
    assert!(recorder.has(|e| matches!(e, E::ExtractionProgress { .. })), "{events:?}");
    assert!(recorder.has(|e| matches!(e, E::Completed { .. })), "{events:?}");

    // The last word must be the outcome, not a stage part way through.
    assert!(
        matches!(events.last(), Some(E::Completed { .. })),
        "the final event must be terminal: {events:?}"
    );

    // Every event carries text a consumer can show without inventing its own.
    for event in &events {
        assert!(!event.message().is_empty(), "{event:?} has no message");
    }
}

#[test]
fn download_progress_reaches_the_full_size() {
    use xpack_core::progress::ProgressEvent as E;

    // A bar that stops at ninety-something percent looks like a hang.
    let world = World::new();
    let package = build_package(world.dir.path(), &world.key, "1.1.0");
    let size = std::fs::metadata(&package).unwrap().len();

    let fixture = Fixture::default();
    fixture.serve(&index_url(), index_json("1.1.0", "demo-1.1.0.xpkg", size));
    fixture.serve_file(&format!("{BASE}/demo-1.1.0.xpkg"), &package);

    let recorder = Recorder::default();
    Updater::new(&world.paths, &fixture).reporting_to(&recorder).update(BASE, &options()).unwrap();

    let highest = recorder
        .events()
        .iter()
        .filter_map(|e| match e {
            E::DownloadProgress { downloaded, .. } => Some(*downloaded),
            _ => None,
        })
        .max()
        .expect("progress must be reported");
    assert_eq!(highest, size, "progress must reach the size actually downloaded");
}

#[test]
fn a_check_reports_what_it_found_without_downloading() {
    use xpack_core::progress::ProgressEvent as E;

    let world = World::new();
    let fixture = Fixture::default();
    fixture.serve(&index_url(), index_json("1.1.0", "demo-1.1.0.xpkg", 4096));

    let recorder = Recorder::default();
    Updater::new(&world.paths, &fixture).reporting_to(&recorder).check(BASE, &options()).unwrap();

    assert!(recorder.has(|e| matches!(e, E::CheckingForUpdate { .. })));
    assert!(recorder.has(|e| matches!(e, E::UpdateAvailable { .. })));
    assert!(!recorder.has(|e| matches!(e, E::DownloadStarted { .. })), "check must not download");
}

#[test]
fn an_installation_already_current_is_reported_as_such() {
    use xpack_core::progress::ProgressEvent as E;

    let world = World::new();
    let older = build_package(world.dir.path(), &world.key, "0.9.0");
    let size = std::fs::metadata(&older).unwrap().len();

    let fixture = Fixture::default();
    fixture.serve(&index_url(), index_json("0.9.0", "demo-0.9.0.xpkg", size));

    let recorder = Recorder::default();
    assert!(
        Updater::new(&world.paths, &fixture)
            .reporting_to(&recorder)
            .check(BASE, &options())
            .unwrap()
            .is_none()
    );

    assert!(recorder.has(|e| matches!(e, E::UpToDate { .. })), "{:?}", recorder.events());
}

#[test]
fn a_server_declaring_no_size_reports_an_unknown_total() {
    use xpack_core::progress::ProgressEvent as E;

    // A total of zero would make a consumer draw a bar against nothing, so an
    // absent size must stay absent rather than becoming Some(0).
    let world = World::new();
    let fixture = Fixture::default();
    fixture.serve(&index_url(), index_json("1.1.0", "demo-1.1.0.xpkg", 0));

    let recorder = Recorder::default();
    let _ = Updater::new(&world.paths, &fixture).reporting_to(&recorder).check(BASE, &options());

    assert!(
        recorder.has(|e| matches!(e, E::UpdateAvailable { total_bytes: None, .. })),
        "{:?}",
        recorder.events()
    );
}

// --- staged rollout ------------------------------------------------------
//
// The brake a publisher needs when a release turns out to be bad. These drive
// the real updater against a real index; the bucketing rule itself is unit
// tested in `xpack_update::rollout`.

/// An index offering `version` to `percent` of installations.
fn staged_index_json(version: &str, file: &str, size: u64, percent: u8) -> Vec<u8> {
    format!(
        r#"{{"application":"com.example.demo","version":"{version}",
            "platform":{{"os":"{}","arch":"{}"}},"channel":"stable",
            "rollout":{percent},
            "package":{{"file":"{file}","size":{size}}}}}"#,
        Platform::host().unwrap().os,
        Platform::host().unwrap().arch
    )
    .into_bytes()
}

/// An installation on 1.0.0, with 1.1.0 served at `percent`.
fn staged_world(percent: Option<u8>) -> (World, Fixture) {
    let world = World::new();
    let package = build_package(world.dir.path(), &world.key, "1.1.0");
    let size = std::fs::metadata(&package).unwrap().len();

    let index = match percent {
        Some(percent) => staged_index_json("1.1.0", "demo-1.1.0.xpkg", size, percent),
        None => index_json("1.1.0", "demo-1.1.0.xpkg", size),
    };

    let fixture = Fixture::default();
    fixture.serve(&index_url(), index);
    fixture.serve_file(&format!("{BASE}/demo-1.1.0.xpkg"), &package);
    (world, fixture)
}

/// The options an unattended updater uses.
fn unattended() -> UpdateOptions {
    UpdateOptions { respect_rollout: true, ..options() }
}

#[test]
fn a_halted_rollout_installs_nothing() {
    // Setting the percentage to zero is how a publisher stops a bad release
    // reaching anybody else, so it must hold for every installation.
    let (world, fixture) = staged_world(Some(0));

    let result = Updater::new(&world.paths, &fixture).update(BASE, &unattended()).unwrap();

    assert_eq!(result, None, "a halted rollout installed a version");
    assert_eq!(world.active(), Some(Version::parse("1.0.0").unwrap()));
}

#[test]
fn a_held_back_installation_downloads_nothing() {
    // The decision is taken before the transfer: an installation outside the
    // rollout must not spend a user's bandwidth on a package it will not
    // install.
    let (world, fixture) = staged_world(Some(0));

    Updater::new(&world.paths, &fixture).update(BASE, &unattended()).unwrap();

    assert!(fixture.was_fetched(&index_url()), "the index should still be read");
    assert!(
        !fixture.was_fetched(&format!("{BASE}/demo-1.1.0.xpkg")),
        "the package was downloaded despite the rollout excluding this installation"
    );
}

#[test]
fn a_full_rollout_installs_normally() {
    // The control: nothing else about this release differs.
    let (world, fixture) = staged_world(Some(100));

    let result = Updater::new(&world.paths, &fixture).update(BASE, &unattended()).unwrap();

    assert_eq!(result, Some(Version::parse("1.1.0").unwrap()));
}

#[test]
fn an_index_without_a_rollout_is_fully_published() {
    // Every index written before staged rollouts existed lacks the field and
    // must keep working exactly as it did.
    let (world, fixture) = staged_world(None);

    let result = Updater::new(&world.paths, &fixture).update(BASE, &unattended()).unwrap();

    assert_eq!(result, Some(Version::parse("1.1.0").unwrap()));
}

#[test]
fn a_manual_check_is_not_held_back_by_a_rollout() {
    // Someone asked for the newest version. "There is one, but not for you"
    // is unhelpful and impossible to explain, so a manual run bypasses it.
    let (world, fixture) = staged_world(Some(0));

    let result = Updater::new(&world.paths, &fixture)
        .update(BASE, &UpdateOptions { respect_rollout: false, ..options() })
        .unwrap();

    assert_eq!(result, Some(Version::parse("1.1.0").unwrap()));
}

#[test]
fn check_reports_nothing_available_when_held_back() {
    // `check` and `update` must agree. A check that advertised a version the
    // updater then refused to install would be worse than either alone.
    let (world, fixture) = staged_world(Some(0));

    let available = Updater::new(&world.paths, &fixture).check(BASE, &unattended()).unwrap();

    assert!(available.is_none(), "check offered a version the rollout excludes");
}

#[test]
fn the_rollout_identifier_is_created_once_and_then_reused() {
    // A value that moved would let an installation drop out of a rollout it
    // had already been offered.
    let (world, fixture) = staged_world(Some(50));
    let updater = Updater::new(&world.paths, &fixture);

    let _ = updater.check(BASE, &unattended()).unwrap();
    let first = world.lock().load_state().unwrap().value.rollout_id;
    assert!(first.is_some(), "no identifier was generated");

    let _ = updater.check(BASE, &unattended()).unwrap();
    let second = world.lock().load_state().unwrap().value.rollout_id;

    assert_eq!(first, second, "the identifier changed between checks");
}

#[test]
fn a_fully_published_release_needs_no_identifier() {
    // An installation that only ever sees ordinary releases never acquires
    // one, so nothing is generated or stored in the common case.
    let (world, fixture) = staged_world(Some(100));

    Updater::new(&world.paths, &fixture).update(BASE, &unattended()).unwrap();

    assert_eq!(world.lock().load_state().unwrap().value.rollout_id, None);
}

// --- deltas --------------------------------------------------------------
//
// A delta is an optimisation, so the tests that matter are not "it is
// smaller" — they are "it produces the same installation" and "when it cannot
// be used, the update still happens".

/// An index offering a delta from `from` alongside the full package.
fn index_with_delta(
    version: &str,
    size: u64,
    from: &str,
    delta_file: &str,
    delta_size: u64,
) -> Vec<u8> {
    format!(
        r#"{{"application":"com.example.demo","version":"{version}",
            "platform":{{"os":"{}","arch":"{}"}},"channel":"stable",
            "package":{{"file":"demo-{version}.xpkg","size":{size}}},
            "deltas":[{{"from":"{from}","file":"{delta_file}","size":{delta_size}}}]}}"#,
        Platform::host().unwrap().os,
        Platform::host().unwrap().arch
    )
    .into_bytes()
}

/// An installation on 1.0.0, with 1.1.0 and a real delta served.
fn delta_world() -> (World, Fixture, PathBuf) {
    let world = World::new();
    let base = world.dir.path().join("demo-1.0.0.xpkg");
    let target = build_package(world.dir.path(), &world.key, "1.1.0");

    let delta_path = world.dir.path().join("1.0.0-to-1.1.0.xpkgd");
    xpack_package::delta::build(&base, &target, &delta_path).expect("the delta should build");

    let full_size = std::fs::metadata(&target).unwrap().len();
    let delta_size = std::fs::metadata(&delta_path).unwrap().len();

    let fixture = Fixture::default();
    fixture.serve(
        &index_url(),
        index_with_delta("1.1.0", full_size, "1.0.0", "1.0.0-to-1.1.0.xpkgd", delta_size),
    );
    fixture.serve_file(&format!("{BASE}/demo-1.1.0.xpkg"), &target);
    fixture.serve_file(&format!("{BASE}/1.0.0-to-1.1.0.xpkgd"), &delta_path);
    (world, fixture, delta_path)
}

#[test]
fn a_delta_is_used_when_one_is_offered_for_the_installed_version() {
    let (world, fixture, _) = delta_world();

    let result = Updater::new(&world.paths, &fixture).update(BASE, &options()).unwrap();

    assert_eq!(result, Some(Version::parse("1.1.0").unwrap()));
    assert!(fixture.was_fetched(&format!("{BASE}/1.0.0-to-1.1.0.xpkgd")), "the delta was not used");
    assert!(
        !fixture.was_fetched(&format!("{BASE}/demo-1.1.0.xpkg")),
        "the full package was downloaded even though the delta worked"
    );
}

#[test]
fn a_delta_update_installs_the_same_files_as_a_full_one() {
    // The claim that makes deltas safe to prefer at all.
    let (world, fixture, _) = delta_world();
    Updater::new(&world.paths, &fixture).update(BASE, &options()).unwrap();

    let from_delta = world.paths.version_dir(&Version::parse("1.1.0").unwrap());

    // Install the same release the ordinary way, elsewhere, and compare.
    let plain = World::new();
    let target = build_package(plain.dir.path(), &plain.key, "1.1.0");
    let other = Fixture::default();
    let size = std::fs::metadata(&target).unwrap().len();
    other.serve(&index_url(), index_json("1.1.0", "demo-1.1.0.xpkg", size));
    other.serve_file(&format!("{BASE}/demo-1.1.0.xpkg"), &target);
    Updater::new(&plain.paths, &other).update(BASE, &options()).unwrap();
    let from_full = plain.paths.version_dir(&Version::parse("1.1.0").unwrap());

    // The payload only. `.xpack/` holds the signed manifest, and these two
    // installations were signed by different keys, so it differs by design.
    let read = |root: &std::path::Path| -> std::collections::BTreeMap<String, Vec<u8>> {
        let mut out = std::collections::BTreeMap::new();
        for entry in walkdir::WalkDir::new(root).sort_by_file_name() {
            let entry = entry.unwrap();
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry.path().strip_prefix(root).unwrap().to_string_lossy().to_string();
            if rel.starts_with(".xpack") {
                continue;
            }
            out.insert(rel, std::fs::read(entry.path()).unwrap());
        }
        out
    };
    assert_eq!(read(&from_delta), read(&from_full), "a delta install differs from a full one");
}

#[test]
fn a_corrupt_delta_falls_back_to_the_full_package() {
    // The property that makes a delta safe to offer: it can never be the
    // reason an update does not happen.
    let (world, fixture, _) = delta_world();
    fixture.serve(&format!("{BASE}/1.0.0-to-1.1.0.xpkgd"), b"not an archive at all".to_vec());

    let result = Updater::new(&world.paths, &fixture).update(BASE, &options()).unwrap();

    assert_eq!(result, Some(Version::parse("1.1.0").unwrap()), "the update should still happen");
    assert!(
        fixture.was_fetched(&format!("{BASE}/demo-1.1.0.xpkg")),
        "the full package was not fetched after the delta failed"
    );
}

#[test]
fn a_delta_signed_by_another_key_falls_back_rather_than_failing() {
    let (world, fixture, _) = delta_world();

    // A delta for the same versions, signed by someone else entirely.
    let attacker = KeyPair::generate().unwrap();
    let evil_dir = tempfile::tempdir().unwrap();
    let evil_base = build_package(evil_dir.path(), &attacker, "1.0.0");
    let evil_target = build_package(evil_dir.path(), &attacker, "1.1.0");
    let evil_delta = evil_dir.path().join("evil.xpkgd");
    xpack_package::delta::build(&evil_base, &evil_target, &evil_delta).unwrap();
    fixture.serve_file(&format!("{BASE}/1.0.0-to-1.1.0.xpkgd"), &evil_delta);

    let result = Updater::new(&world.paths, &fixture).update(BASE, &options()).unwrap();

    assert_eq!(result, Some(Version::parse("1.1.0").unwrap()));
    assert!(
        fixture.was_fetched(&format!("{BASE}/demo-1.1.0.xpkg")),
        "an unsigned-for-us delta must fall back, not install"
    );
}

#[test]
fn a_delta_for_a_version_that_is_not_installed_is_ignored() {
    // The index offers a delta from 0.9.0; 1.0.0 is installed. Downloading it
    // would waste the user's bandwidth on something that cannot apply.
    let world = World::new();
    let target = build_package(world.dir.path(), &world.key, "1.1.0");
    let size = std::fs::metadata(&target).unwrap().len();

    let fixture = Fixture::default();
    fixture.serve(&index_url(), index_with_delta("1.1.0", size, "0.9.0", "other.xpkgd", 10));
    fixture.serve_file(&format!("{BASE}/demo-1.1.0.xpkg"), &target);

    let result = Updater::new(&world.paths, &fixture).update(BASE, &options()).unwrap();

    assert_eq!(result, Some(Version::parse("1.1.0").unwrap()));
    assert!(
        !fixture.was_fetched(&format!("{BASE}/other.xpkgd")),
        "an inapplicable delta was fetched"
    );
}

#[test]
fn a_delta_whose_base_was_pruned_falls_back() {
    // `prune` removes old versions, and a delta needs its base on disk.
    let (world, fixture, _) = delta_world();
    let base_dir = world.paths.version_dir(&Version::parse("1.0.0").unwrap());
    // The file the delta expects to reuse rather than carry.
    std::fs::remove_file(base_dir.join("runtime/lib.bin")).unwrap();

    let result = Updater::new(&world.paths, &fixture).update(BASE, &options()).unwrap();

    assert_eq!(result, Some(Version::parse("1.1.0").unwrap()));
    assert!(fixture.was_fetched(&format!("{BASE}/demo-1.1.0.xpkg")));
}
