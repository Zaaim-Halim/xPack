//! Installing onto a machine that has never heard of xPack.
//!
//! The unit tests cover the trailer and the entry-name rules. These cover the
//! thing that actually has to work: a payload assembled by the build side,
//! found by the stub, and turned into a working installation.

use std::path::{Path, PathBuf};

use xpack_core::{Manifest, Platform, Version};
use xpack_installer::Payload;
use xpack_installer::bundle::{self, InstallPlan, Source, TRAILER_LEN, Trailer};
use xpack_package::PackageBuilder;
use xpack_security::KeyPair;

/// Builds a real signed package whose payload is a runnable script.
fn build_package(dir: &Path, key: &KeyPair) -> PathBuf {
    let payload = dir.join("src");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    let app = payload.join("bin/app");
    std::fs::write(&app, "#!/bin/sh\necho installed-and-running\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let manifest = Manifest {
        format_version: xpack_core::FormatVersion::default(),
        application: xpack_core::Application {
            id: "com.example.demo".into(),
            name: "Demo".into(),
            version: Version::parse("1.0.0").unwrap(),
            description: None,
            publisher: None,
        },
        platform: Platform::host().unwrap(),
        launch: xpack_core::LaunchSpec {
            executable: "bin/app".into(),
            arguments: Vec::new(),
            working_directory: None,
            environment: std::collections::BTreeMap::new(),
        },
        update: xpack_core::UpdateSpec::default(),
        health: xpack_core::HealthSpec::default(),
        desktop: xpack_core::DesktopSpec::default(),
        payload: xpack_core::PayloadSpec::default(),
        signing_key: None,
        created_at: None,
    };

    let out = dir.join("app.xpkg");
    PackageBuilder::new(&payload, manifest).build(&out, key).unwrap();
    out
}

/// A stand-in for a runtime binary.
fn fake_binary(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
    path
}

fn plan(key: &KeyPair) -> InstallPlan {
    InstallPlan {
        format_version: InstallPlan::CURRENT_VERSION,
        application_id: "com.example.demo".into(),
        application_name: "Demo".into(),
        version: "1.0.0".into(),
        signing_key: key.public().to_hex(),
        activate: true,
    }
}

/// Builds a payload the way `xpack installer` does.
fn payload_bytes(dir: &Path, key: &KeyPair) -> Vec<u8> {
    let package = build_package(dir, key);
    // What `xpack installer` puts in a real payload. The windowed launcher is
    // Windows-only there, and included here on every platform so that the one
    // machine able to check what Windows does with it is not also the only
    // machine that has it.
    let binaries = vec![
        fake_binary(dir, "xpack-launcher"),
        fake_binary(dir, "xpack-launcherw"),
        fake_binary(dir, "xpack-updater"),
        fake_binary(dir, "xpack-uninstaller"),
    ];
    bundle::build(&plan(key), &package, &binaries).unwrap()
}

#[test]
fn a_payload_appended_to_a_stub_is_found_and_read_back() {
    // The Windows and Linux layout. It cannot be exercised natively here —
    // the stub would have to be a foreign binary — so the mechanism is tested
    // against a stand-in file, which is the part that could be wrong.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let payload = payload_bytes(dir.path(), &key);

    let installer = dir.path().join("installer");
    let mut bytes = b"pretend this is an executable".to_vec();
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(
        &Trailer {
            payload_len: payload.len() as u64,
            payload_sha256: xpack_security::sha256(&payload),
        }
        .to_bytes(),
    );
    std::fs::write(&installer, &bytes).unwrap();

    let source = bundle::locate(&installer).unwrap();
    assert!(matches!(source, Source::Appended { .. }), "got {source:?}");
    assert_eq!(bundle::read(&installer, &source).unwrap(), payload);
}

#[test]
fn a_truncated_installer_is_refused_rather_than_half_installed() {
    // A download that stopped early. Failing loudly here is the difference
    // between a clear message and a confusing archive error mid-install.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let payload = payload_bytes(dir.path(), &key);

    let installer = dir.path().join("installer");
    let mut bytes = b"stub".to_vec();
    // One byte short of what the trailer will claim.
    bytes.extend_from_slice(&payload[..payload.len() - 1]);
    bytes.extend_from_slice(
        &Trailer {
            payload_len: payload.len() as u64,
            payload_sha256: xpack_security::sha256(&payload),
        }
        .to_bytes(),
    );
    std::fs::write(&installer, &bytes).unwrap();

    let source = bundle::locate(&installer).unwrap();
    assert!(bundle::read(&installer, &source).is_err(), "a short payload was accepted");
}

#[test]
fn a_tampered_payload_fails_its_checksum() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let payload = payload_bytes(dir.path(), &key);

    let installer = dir.path().join("installer");
    let mut bytes = b"stub".to_vec();
    let mut altered = payload.clone();
    let last = altered.len() - 1;
    altered[last] ^= 0xFF;
    bytes.extend_from_slice(&altered);
    bytes.extend_from_slice(
        &Trailer {
            payload_len: payload.len() as u64,
            payload_sha256: xpack_security::sha256(&payload),
        }
        .to_bytes(),
    );
    std::fs::write(&installer, &bytes).unwrap();

    let source = bundle::locate(&installer).unwrap();
    let err = bundle::read(&installer, &source).unwrap_err();
    assert!(err.to_string().contains("checksum"), "got {err}");
}

#[test]
fn a_stub_with_no_payload_says_so_plainly() {
    let dir = tempfile::tempdir().unwrap();
    let stub = dir.path().join("installer");
    std::fs::write(&stub, vec![0u8; TRAILER_LEN * 2]).unwrap();

    let err = bundle::locate(&stub).unwrap_err();
    assert!(err.to_string().contains("unbuilt stub"), "got {err}");
}

#[test]
fn a_payload_beside_the_stub_is_found() {
    // The macOS layout, where the executable is left pristine so it can be
    // signed and notarised.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let payload = payload_bytes(dir.path(), &key);

    let macos = dir.path().join("Demo.app/Contents/MacOS");
    let resources = dir.path().join("Demo.app/Contents/Resources");
    std::fs::create_dir_all(&macos).unwrap();
    std::fs::create_dir_all(&resources).unwrap();

    let stub = macos.join("Demo");
    std::fs::write(&stub, b"a pristine executable").unwrap();
    std::fs::write(resources.join(bundle::SIDECAR_NAME), &payload).unwrap();

    let source = bundle::locate(&stub).unwrap();
    assert_eq!(source, Source::Sidecar(resources.join(bundle::SIDECAR_NAME)));
    assert_eq!(bundle::read(&stub, &source).unwrap(), payload);
}

#[test]
fn a_payload_installs_a_working_application() {
    // The claim the whole crate exists to make.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let payload = payload_bytes(dir.path(), &key);

    let root = dir.path().join("root");
    let outcome = Payload::unpack(&payload).unwrap().install_into(&root).unwrap();

    assert_eq!(outcome.version, Version::parse("1.0.0").unwrap());
    assert!(outcome.activated);
    assert!(outcome.launcher.is_file(), "no launcher was placed");
    assert!(root.join("com.example.demo/state/state.json").is_file(), "no state was written");

    // The key travelled in the payload and is pinned into the installation, so
    // every later update is held to the same publisher.
    let trust = std::fs::read_to_string(root.join("com.example.demo/config/trust.json")).unwrap();
    assert!(trust.contains(&key.public().to_hex()), "the publisher's key was not pinned");
}

#[test]
fn a_package_signed_by_another_key_is_refused() {
    // The installer pins the publisher's key rather than trusting whatever
    // signed the download, so a substituted package cannot install.
    let dir = tempfile::tempdir().unwrap();
    let real = KeyPair::generate().unwrap();
    let attacker = KeyPair::generate().unwrap();

    let package = build_package(dir.path(), &attacker);
    let binaries = vec![fake_binary(dir.path(), "xpack-launcher")];
    // A plan naming the real publisher, with a package signed by someone else.
    let payload = bundle::build(&plan(&real), &package, &binaries).unwrap();

    let err =
        Payload::unpack(&payload).unwrap().install_into(&dir.path().join("root")).unwrap_err();

    assert!(err.is_integrity_failure(), "expected an integrity failure, got {err}");
}
