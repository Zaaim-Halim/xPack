//! Differential updates, against real signed packages.
//!
//! The claim to prove is not "a delta is smaller". It is that assembling one
//! produces exactly what installing the full package produces, and that a
//! delta which lies about anything fails instead of installing something else.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use xpack_core::{Manifest, Platform, Version};
use xpack_package::{PackageBuilder, PackageReader, delta};
use xpack_security::KeyPair;

/// A payload with one large file that never changes and one small file that does.
///
/// This is the shape the whole feature exists for: a bundled runtime that is
/// byte-identical between releases, and an application file that is not.
fn write_payload(dir: &Path, version: &str, runtime_bytes: &[u8]) -> PathBuf {
    let root = dir.join(format!("src-{version}"));
    std::fs::create_dir_all(root.join("runtime/lib")).unwrap();
    std::fs::create_dir_all(root.join("application")).unwrap();

    std::fs::write(root.join("runtime/lib/big.so"), runtime_bytes).unwrap();
    std::fs::write(root.join("runtime/lib/small.so"), b"unchanging support library").unwrap();
    std::fs::write(root.join("application/app.jar"), format!("application code for {version}"))
        .unwrap();

    // An executable that does not change between releases, so it is reused
    // rather than carried — which is exactly the file whose mode a reuse
    // path could quietly drop.
    std::fs::create_dir_all(root.join("runtime/bin")).unwrap();
    let launcher = root.join("runtime/bin/java");
    std::fs::write(&launcher, b"#!/bin/sh\nexec true\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    root
}

fn manifest(version: &str) -> Manifest {
    Manifest {
        format_version: xpack_core::FormatVersion::default(),
        application: xpack_core::Application {
            id: "com.example.demo".into(),
            name: "Demo".into(),
            version: Version::parse(version).unwrap(),
            description: None,
            publisher: None,
        },
        platform: Platform::host().unwrap(),
        launch: xpack_core::LaunchSpec {
            executable: "application/app.jar".into(),
            arguments: Vec::new(),
            working_directory: None,
            keep_working_directory: false,
            environment: BTreeMap::new(),
        },
        update: xpack_core::UpdateSpec::default(),
        health: xpack_core::HealthSpec::default(),
        desktop: xpack_core::DesktopSpec::default(),
        payload: xpack_core::PayloadSpec::default(),
        signing_key: None,
        created_at: None,
        command: None,
        commands: Vec::new(),
    }
}

/// Builds a signed package for `version`; `runtime_bytes` decides whether the
/// large file is the same as another version's.
fn build_package(dir: &Path, key: &KeyPair, version: &str, runtime_bytes: &[u8]) -> PathBuf {
    let payload = write_payload(dir, version, runtime_bytes);
    let out = dir.join(format!("demo-{version}.xpkg"));
    PackageBuilder::new(&payload, manifest(version)).build(&out, key).unwrap();
    out
}

/// Deterministic bytes that do not compress, standing in for a real runtime.
///
/// A plain xorshift: reproducible across runs and platforms, so a failure is
/// always the same failure, and dense enough that the archive cannot shrink it.
fn incompressible(len: usize) -> Vec<u8> {
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// Every file under `root`, as path -> bytes, for comparing whole trees.
fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root).sort_by_file_name() {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            let relative =
                entry.path().strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            out.insert(relative, std::fs::read(entry.path()).unwrap());
        }
    }
    out
}

/// An installed 1.0.0, plus the packages for both versions.
struct World {
    _dir: tempfile::TempDir,
    key: KeyPair,
    base_package: PathBuf,
    target_package: PathBuf,
    installed_base: PathBuf,
    root: PathBuf,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let key = KeyPair::generate().unwrap();
        // A megabyte of runtime, byte-identical across both versions, and
        // deliberately incompressible. A megabyte of one repeated byte would
        // shrink to nothing in the archive, and the size comparison below
        // would then be measuring the compressor rather than the delta.
        let runtime = incompressible(1024 * 1024);

        let base_package = build_package(dir.path(), &key, "1.0.0", &runtime);
        let target_package = build_package(dir.path(), &key, "1.1.0", &runtime);

        // Install 1.0.0 the ordinary way, so the base is a real installation.
        let installed_base = dir.path().join("installed/1.0.0");
        PackageReader::open(&base_package)
            .unwrap()
            .verify_with_keys(&[key.public()])
            .unwrap()
            .extract_to(&installed_base)
            .unwrap();

        let root = dir.path().to_path_buf();
        Self { _dir: dir, key, base_package, target_package, installed_base, root }
    }

    fn build_delta(&self) -> (PathBuf, delta::BuiltDelta) {
        let out = self.root.join("1.0.0-to-1.1.0.xpkgd");
        let built = delta::build(&self.base_package, &self.target_package, &out).unwrap();
        (out, built)
    }

    fn open_delta(&self, path: &Path) -> xpack_package::VerifiedDelta {
        PackageReader::open(path).unwrap().verify_delta_with_keys(&[self.key.public()]).unwrap()
    }
}

#[test]
fn assembling_a_delta_produces_exactly_what_the_full_package_would() {
    // The claim the feature lives or dies on.
    let world = World::new();
    let (path, _) = world.build_delta();

    let assembled = world.root.join("assembled");
    world
        .open_delta(&path)
        .assemble_to(&assembled, &world.installed_base)
        .expect("a well-formed delta should assemble");

    let expected = world.root.join("full");
    PackageReader::open(&world.target_package)
        .unwrap()
        .verify_with_keys(&[world.key.public()])
        .unwrap()
        .extract_to(&expected)
        .unwrap();

    assert_eq!(tree(&assembled), tree(&expected), "the assembled tree differs from a full install");
}

#[test]
fn the_unchanged_runtime_is_not_carried_in_the_delta() {
    // The point of the exercise: a megabyte of unchanged runtime should not be
    // shipped to deliver a change to one small file.
    let world = World::new();
    let (_, built) = world.build_delta();

    assert_eq!(built.changed, 1, "only the application file changed");
    assert_eq!(built.reused, 3, "every runtime file should be reused");
    assert!(
        built.size * 10 < built.full_size,
        "the delta is {} bytes against a full package of {}; it should be far smaller",
        built.size,
        built.full_size
    );
}

#[test]
fn a_delta_carries_the_targets_signature_unchanged() {
    // It must be byte-identical: a re-serialised manifest would no longer
    // verify, and the delta's whole security argument is that the signature
    // over the target manifest still holds.
    let world = World::new();
    let (path, _) = world.build_delta();

    let delta = world.open_delta(&path);
    let full = PackageReader::open(&world.target_package)
        .unwrap()
        .verify_with_keys(&[world.key.public()])
        .unwrap();

    assert_eq!(delta.manifest_bytes(), full.manifest_bytes());
    assert_eq!(delta.signature().to_hex(), full.signature().to_hex());
    assert_eq!(delta.manifest().application.version, Version::parse("1.1.0").unwrap());
}

#[test]
fn a_delta_knows_which_version_it_applies_to() {
    let world = World::new();
    let (path, _) = world.build_delta();
    let delta = world.open_delta(&path);

    assert_eq!(delta.base_version(), &Version::parse("1.0.0").unwrap());
    assert!(delta.ensure_applies_to(&Version::parse("1.0.0").unwrap()).is_ok());

    // Checked against what is installed, never the other way round.
    let err = delta.ensure_applies_to(&Version::parse("0.9.0").unwrap()).unwrap_err();
    assert!(err.to_string().contains("0.9.0 is installed"), "got {err}");
}

#[test]
fn a_delta_signed_by_another_key_is_refused() {
    let world = World::new();
    let (path, _) = world.build_delta();
    let attacker = KeyPair::generate().unwrap();

    let result = PackageReader::open(&path).unwrap().verify_delta_with_keys(&[attacker.public()]);
    assert!(result.is_err(), "a delta must verify against the publisher's key like any package");
}

#[test]
fn a_tampered_base_file_fails_instead_of_being_installed() {
    // The reused half of the security argument. A file taken from disk is
    // hashed as it is copied, against the *target* manifest, so an installed
    // version someone edited cannot be carried into the new one.
    let world = World::new();
    let (path, _) = world.build_delta();

    std::fs::write(world.installed_base.join("runtime/lib/small.so"), b"tampered").unwrap();

    let assembled = world.root.join("assembled");
    let err = world
        .open_delta(&path)
        .assemble_to(&assembled, &world.installed_base)
        .expect_err("a tampered base file must not be carried into the new version");

    assert!(err.is_integrity_failure(), "expected an integrity failure, got {err}");
    assert!(!assembled.exists(), "a partial tree was left behind at {}", assembled.display());
}

#[test]
fn a_missing_base_file_fails_rather_than_producing_a_short_install() {
    // What `prune` removing the base version looks like, in miniature.
    let world = World::new();
    let (path, _) = world.build_delta();

    std::fs::remove_file(world.installed_base.join("runtime/lib/big.so")).unwrap();

    let assembled = world.root.join("assembled");
    let err = world
        .open_delta(&path)
        .assemble_to(&assembled, &world.installed_base)
        .expect_err("a missing base file must fail");

    assert!(err.to_string().contains("installed version"), "got {err}");
    assert!(!assembled.exists(), "a partial tree was left behind");
}

#[test]
fn a_delta_cannot_be_built_between_different_applications() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let runtime = vec![1u8; 64];

    let base = build_package(dir.path(), &key, "1.0.0", &runtime);

    let payload = write_payload(dir.path(), "other", &runtime);
    let mut other = manifest("1.1.0");
    other.application.id = "com.example.other".into();
    let target = dir.path().join("other.xpkg");
    PackageBuilder::new(&payload, other).build(&target, &key).unwrap();

    let err = delta::build(&base, &target, &dir.path().join("d.xpkgd")).unwrap_err();
    assert!(err.to_string().contains("different applications"), "got {err}");
}

#[test]
fn a_delta_cannot_be_built_from_a_version_to_itself() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let runtime = vec![2u8; 64];
    let package = build_package(dir.path(), &key, "1.0.0", &runtime);

    let err = delta::build(&package, &package, &dir.path().join("d.xpkgd")).unwrap_err();
    assert!(err.to_string().contains("both packages are version"), "got {err}");
}

#[test]
fn a_full_package_is_not_accepted_as_a_delta() {
    // It carries no plan, so there is nothing to say which version it applies
    // to. Treating it as a delta would mean assembling against an arbitrary
    // base.
    let world = World::new();
    let result = PackageReader::open(&world.target_package)
        .unwrap()
        .verify_delta_with_keys(&[world.key.public()]);
    assert!(result.is_err(), "a package with no delta plan must not verify as a delta");
}

#[test]
fn a_delta_is_not_accepted_as_a_full_package_and_says_what_it_is() {
    // Read as a package, the delta's plan is an entry outside the payload,
    // which used to be reported as an unsafe entry: a security failure for
    // what is only the wrong kind of file.
    let world = World::new();
    let (path, _) = world.build_delta();

    let reader = PackageReader::open(&path).unwrap();
    assert!(reader.is_delta());
    let error = reader.verify_with_keys(&[world.key.public()]).unwrap_err();

    assert!(!error.is_integrity_failure(), "reported as a security failure: {error}");
    let message = error.to_string();
    assert!(message.contains("is a delta (an update from 1.0.0), not a full package"), "{message}");
    assert!(message.contains("xpack index --delta"), "{message}");
}

#[test]
fn a_full_package_is_not_mistaken_for_a_delta() {
    let world = World::new();
    let mut reader = PackageReader::open(&world.target_package).unwrap();
    assert!(!reader.is_delta());
    reader.ensure_full_package().unwrap();
}

#[test]
#[cfg(unix)]
fn a_reused_executable_keeps_its_permission_bits() {
    // A reused file is adopted from the installed version rather than
    // written out, and adopting it must still leave the mode the manifest
    // asks for. Losing the executable bit here would install an application
    // that verifies perfectly and cannot start.
    use std::os::unix::fs::PermissionsExt;

    let world = World::new();
    let (path, _) = world.build_delta();

    let assembled = world.root.join("assembled");
    world.open_delta(&path).assemble_to(&assembled, &world.installed_base).unwrap();

    let mode = std::fs::metadata(assembled.join("runtime/bin/java")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755, "the executable bit was lost: mode {mode:o}");
}

#[test]
fn adopting_a_reused_file_does_not_disturb_the_version_it_came_from() {
    // Whatever mechanism the filesystem allowed, the installed version this
    // was assembled against has to be exactly as it was: it is still the
    // version a user is running, and still the one a rollback returns to.
    let world = World::new();
    let before = tree(&world.installed_base);

    let (path, _) = world.build_delta();
    let assembled = world.root.join("assembled");
    world.open_delta(&path).assemble_to(&assembled, &world.installed_base).unwrap();

    assert_eq!(tree(&world.installed_base), before, "the base version was modified");
}
