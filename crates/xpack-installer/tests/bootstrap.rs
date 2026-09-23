//! Installing onto a machine that has never heard of xPack.
//!
//! The unit tests cover the trailer and the entry-name rules. These cover the
//! thing that actually has to work: a payload assembled by the build side,
//! found by the stub, and turned into a working installation.

use std::path::{Path, PathBuf};

use xpack_core::{Manifest, Platform, Version};
use xpack_install::Existing;
use xpack_installer::bundle::{self, InstallPlan, Source, TRAILER_LEN, Trailer};
use xpack_installer::{Payload, Request};
use xpack_package::PackageBuilder;
use xpack_security::KeyPair;

/// Builds a real signed package whose payload is a runnable script.
fn build_package(dir: &Path, key: &KeyPair) -> PathBuf {
    build_package_for(dir, key, Platform::host().unwrap())
}

/// The same package, built for a chosen platform.
fn build_package_for(dir: &Path, key: &KeyPair, platform: Platform) -> PathBuf {
    build_package_with(dir, key, platform, None)
}

/// The same package, naming an icon in its payload.
fn build_package_with(
    dir: &Path,
    key: &KeyPair,
    platform: Platform,
    icon: Option<&[u8]>,
) -> PathBuf {
    let payload = dir.join("src");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    let app = payload.join("bin/app");
    std::fs::write(&app, "#!/bin/sh\necho installed-and-running\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    if let Some(icon) = icon {
        std::fs::write(payload.join("icon.png"), icon).unwrap();
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
        platform,
        launch: xpack_core::LaunchSpec {
            executable: "bin/app".into(),
            arguments: Vec::new(),
            working_directory: None,
            keep_working_directory: false,
            environment: std::collections::BTreeMap::new(),
        },
        update: xpack_core::UpdateSpec::default(),
        health: xpack_core::HealthSpec::default(),
        desktop: xpack_core::DesktopSpec {
            icon: icon.map(|_| "icon.png".to_string()),
            ..xpack_core::DesktopSpec::default()
        },
        payload: xpack_core::PayloadSpec::default(),
        signing_key: None,
        created_at: None,
        command: None,
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
        ui: None,
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
    let outcome = Payload::unpack(&payload)
        .unwrap()
        .verify()
        .unwrap()
        .install_into(&Request::new(root.clone()), &xpack_core::NoProgress)
        .unwrap();

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

    let Err(err) = Payload::unpack(&payload).unwrap().verify() else {
        panic!("a package signed by another key was verified");
    };

    // Refused before anything could be shown or installed.
    assert!(err.is_integrity_failure(), "expected an integrity failure, got {err}");
    assert!(!dir.path().join("root").exists(), "something was installed");
}

#[test]
fn a_plan_naming_another_application_is_refused() {
    // The plan is not signed. Its application id once chose the directory an
    // install went into, so a rewritten plan could have put a genuine package
    // into another application's installation.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let package = build_package(dir.path(), &key);
    let binaries = vec![fake_binary(dir.path(), "xpack-launcher")];

    let mut rewritten = plan(&key);
    rewritten.application_id = "com.example.other".into();
    let payload = bundle::build(&rewritten, &package, &binaries).unwrap();

    let Err(err) = Payload::unpack(&payload).unwrap().verify() else {
        panic!("a plan naming another application was accepted");
    };
    assert!(err.is_integrity_failure(), "expected an integrity failure, got {err}");
}

#[test]
fn what_is_shown_comes_from_the_signed_package_not_the_plan() {
    // A rewritten plan can say anything; only the manifest is signed.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let package = build_package(dir.path(), &key);
    let binaries = vec![fake_binary(dir.path(), "xpack-launcher")];

    let mut rewritten = plan(&key);
    rewritten.application_name = "Totally Trustworthy Bank".into();
    rewritten.version = "99.0.0".into();
    let payload = bundle::build(&rewritten, &package, &binaries).unwrap();

    let verified = Payload::unpack(&payload).unwrap().verify().unwrap();
    assert_eq!(verified.manifest().application.name, "Demo");
    assert_eq!(verified.manifest().application.version, Version::parse("1.0.0").unwrap());
}

#[test]
fn a_package_for_another_platform_is_refused_before_anything_is_shown() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    // The same system with the other architecture: never this machine, and
    // always one this machine can build for. Another operating system is not:
    // a Windows host cannot record the Unix executable bit, so it refuses to
    // build a Linux or macOS package at all, and the test would fail on that
    // refusal without reaching the check it is here for.
    let host = Platform::host().unwrap();
    let other = match host.arch {
        xpack_core::Arch::X64 => xpack_core::Arch::Arm64,
        xpack_core::Arch::Arm64 => xpack_core::Arch::X64,
    };
    let package = build_package_for(dir.path(), &key, Platform { os: host.os, arch: other });
    let binaries = vec![fake_binary(dir.path(), "xpack-launcher")];
    let payload = bundle::build(&plan(&key), &package, &binaries).unwrap();

    assert!(Payload::unpack(&payload).unwrap().verify().is_err());
}

#[test]
fn looking_first_matches_installing_then() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let payload = payload_bytes(dir.path(), &key);
    let root = dir.path().join("root");
    let verified = Payload::unpack(&payload).unwrap().verify().unwrap();

    assert_eq!(verified.inspect(&root).unwrap(), Existing::Nothing);
    assert!(!root.exists(), "looking created the root");

    verified.install_into(&Request::new(root.clone()), &xpack_core::NoProgress).unwrap();

    assert_eq!(verified.inspect(&root).unwrap(), Existing::Installed);
    let again = verified.install_into(&Request::new(root), &xpack_core::NoProgress);
    assert!(again.is_err(), "the same version installed twice");
}

#[test]
fn the_licence_travels_in_the_payload_and_the_icon_comes_from_the_package() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let icon: &[u8] = b"the icon's bytes";
    let package = build_package_with(dir.path(), &key, Platform::host().unwrap(), Some(icon));
    let binaries = vec![fake_binary(dir.path(), "xpack-launcher")];

    let mut with_ui = plan(&key);
    with_ui.ui = Some(xpack_installer::UiPlan::default());
    let payload =
        bundle::build_with_licence(&with_ui, &package, &binaries, Some("Terms.")).unwrap();

    let verified = Payload::unpack(&payload).unwrap().verify().unwrap();
    assert_eq!(verified.licence(), Some("Terms."));
    let found = verified.icon().expect("the package's icon");
    assert_eq!(found.name, "icon.png");
    assert_eq!(found.bytes, icon);
}

#[test]
fn without_settings_the_wizard_is_the_recommended_one() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let verified = Payload::unpack(&payload_bytes(dir.path(), &key)).unwrap().verify().unwrap();
    assert_eq!(verified.ui(), xpack_installer::UiPlan::default());
    assert_eq!(verified.licence(), None);
    assert!(verified.icon().is_none());
}

#[test]
fn a_licence_rewritten_into_something_unreadable_is_refused() {
    // The payload is not signed, so the licence is held to its rules again
    // when it is unpacked, not only when it was built.
    use std::io::Write;

    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let package = build_package(dir.path(), &key);
    let binaries = vec![fake_binary(dir.path(), "xpack-launcher")];
    let honest = bundle::build(&plan(&key), &package, &binaries).unwrap();

    for licence in [vec![0xff, 0xfe, 0x00], vec![b'x'; bundle::MAX_LICENCE_BYTES + 1]] {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(honest.clone())).unwrap();
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for index in 0..archive.len() {
            writer.raw_copy_file(archive.by_index(index).unwrap()).unwrap();
        }
        writer.start_file(bundle::LICENCE_ENTRY, zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(&licence).unwrap();
        let forged = writer.finish().unwrap().into_inner();

        assert!(
            Payload::unpack(&forged).is_err(),
            "a licence of {} bytes was accepted",
            licence.len()
        );
    }
}

#[test]
fn an_icon_altered_inside_the_package_refuses_the_install() {
    use std::io::Write;

    // The icon is covered by the package signature; a package whose icon no
    // longer matches it has been tampered with.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let package =
        build_package_with(dir.path(), &key, Platform::host().unwrap(), Some(b"honest icon"));

    let bytes = std::fs::read(&package).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for index in 0..archive.len() {
        let entry = archive.by_index(index).unwrap();
        if entry.name() == "payload/icon.png" {
            drop(entry);
            writer
                .start_file("payload/icon.png", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"forged icon").unwrap();
        } else {
            writer.raw_copy_file(entry).unwrap();
        }
    }
    let forged_package = dir.path().join("forged.xpkg");
    std::fs::write(&forged_package, writer.finish().unwrap().into_inner()).unwrap();

    let binaries = vec![fake_binary(dir.path(), "xpack-launcher")];
    let payload = bundle::build(&plan(&key), &forged_package, &binaries).unwrap();
    let Err(err) = Payload::unpack(&payload).unwrap().verify() else {
        panic!("a package with a forged icon was verified");
    };
    assert!(err.is_integrity_failure(), "got {err}");
}

// --- the installer as the wizard's engine ------------------------------------

#[test]
fn the_wizard_is_told_what_the_console_would_find() {
    use xpack_installer_ui::{Engine, RootProblem};

    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let verified = Payload::unpack(&payload_bytes(dir.path(), &key)).unwrap().verify().unwrap();
    let root = dir.path().join("root");

    let fresh = Engine::inspect(&verified, &root);
    assert_eq!(fresh.verdict, Ok(Existing::Nothing));
    assert_eq!(fresh.target, root.join("com.example.demo"));
    assert!(!root.exists(), "looking created the root");

    assert_eq!(
        Engine::inspect(&verified, Path::new("relative/root")).verdict,
        Err(RootProblem::NotAbsolute)
    );
}

#[test]
fn the_wizard_installs_and_launches_through_the_same_call() {
    use xpack_installer_ui::{Choices, Engine};

    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let verified = Payload::unpack(&payload_bytes(dir.path(), &key)).unwrap().verify().unwrap();
    let root = dir.path().join("root");

    // Nothing to start before anything is installed.
    assert!(Engine::launch(&verified, &root).is_err());

    let choices = Choices { root: root.clone(), desktop_entry: None, command: None };
    let installed = Engine::install(&verified, &choices, &xpack_core::NoProgress).unwrap();
    assert_eq!(installed.directory, root.join("com.example.demo"));
    assert_eq!(installed.version, Version::parse("1.0.0").unwrap());
    assert!(!installed.shortcut_added, "the package asks for no entry");

    // What the console would now say: installed, so not again.
    assert_eq!(Engine::inspect(&verified, &root).verdict, Ok(Existing::Installed));

    // The launchers in this payload are stand-in shell scripts, which Unix
    // runs and Windows refuses: only a real executable starts there. What is
    // checked everywhere is that launching finds the installed launcher.
    let launched = Engine::launch(&verified, &root);
    #[cfg(unix)]
    launched.expect("the installed launcher starts");
    #[cfg(not(unix))]
    if let Err(error) = launched {
        assert!(!error.to_string().contains("is not installed"), "{error}");
    }
}

#[cfg(unix)]
#[test]
fn a_folder_nobody_may_write_to_is_refused_before_installing() {
    use std::os::unix::fs::PermissionsExt;
    use xpack_installer_ui::{Engine, RootProblem};

    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let verified = Payload::unpack(&payload_bytes(dir.path(), &key)).unwrap().verify().unwrap();

    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();

    let verdict = Engine::inspect(&verified, &locked.join("apps")).verdict;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(verdict, Err(RootProblem::NotWritable));
}
