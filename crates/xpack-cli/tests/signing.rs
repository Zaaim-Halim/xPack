//! `xpack installer --sign-command`: the publisher's own signing command, run
//! on the finished Windows installer.
//!
//! The stub is the minimal PE32+ that `xpack-installer`'s signed fixtures were
//! built from, so these run on every machine. Signing for real needs
//! `osslsigncode` and `openssl` on the PATH; without them those tests say they
//! were skipped rather than passing quietly. The failure paths need neither:
//! they run commands every build machine has, this suite's own `xpack`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn xpack() -> Command {
    Command::new(env!("CARGO_BIN_EXE_xpack"))
}

fn run(command: &mut Command) -> String {
    let out = command.output().expect("xpack should run");
    assert!(out.status.success(), "{:?} failed: {}", command, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A minimal PE32+ executable: the headers and one section of `unsigned.exe`,
/// without the payload that fixture carries.
fn stub(dir: &Path) -> PathBuf {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../xpack-installer/tests/signed/unsigned.exe");
    let bytes = std::fs::read(fixture).unwrap();
    let path = dir.join("stub.exe");
    std::fs::write(&path, &bytes[..1024]).unwrap();
    path
}

/// A package for `os` whose payload is a stand-in for a real application.
fn package(dir: &Path, os: &str) -> PathBuf {
    let payload = dir.join("payload");
    std::fs::create_dir_all(&payload).unwrap();
    std::fs::write(payload.join("app.exe"), b"an application").unwrap();

    let config = dir.join("xpack.json");
    std::fs::write(
        &config,
        format!(
            r#"{{
              "application": {{
                "id": "com.example.demo",
                "name": "My App",
                "version": "1.2.3",
                "publisher": "Example Ltd"
              }},
              "platform": {{ "os": "{os}", "arch": "x64" }},
              "launch": {{ "executable": "app.exe" }}
            }}"#
        ),
    )
    .unwrap();

    run(xpack().args(["keygen", "--out"]).arg(dir.join("key.json")));
    run(xpack()
        .arg("pack")
        .arg(&payload)
        .arg("--config")
        .arg(&config)
        .arg("--key")
        .arg(dir.join("key.json"))
        .arg("--out-dir")
        .arg(dir));

    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "xpkg"))
        .expect("a package was written")
}

/// Builds a Windows installer at `dir/Setup.exe` with `sign_command`.
fn build(dir: &Path, sign_command: &str) -> (PathBuf, Output) {
    let package = package(dir, "windows");
    let stub = stub(dir);
    let installer = dir.join("Setup.exe");
    let output = xpack()
        .arg("installer")
        .arg(&package)
        .arg("--stub")
        .arg(&stub)
        // An unrecognised name, so it is carried through rather than branded.
        .arg("--binary")
        .arg(&stub)
        .arg("--out")
        .arg(&installer)
        .arg("--sign-command")
        .arg(sign_command)
        .arg("--json")
        .output()
        .unwrap();
    (installer, output)
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A throwaway certificate and key, made with `openssl`, when `osslsigncode`
/// is there to sign with them.
fn signing_tools(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let available = |tool: &str| Command::new(tool).arg("--version").output().is_ok();
    if !available("osslsigncode") || !available("openssl") {
        eprintln!("skipped: needs osslsigncode and openssl on the PATH");
        return None;
    }
    let (certificate, key) = (dir.join("test.crt"), dir.join("test.key"));
    let made = Command::new("openssl")
        .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1"])
        .args(["-subj", "/CN=xPack Test Publisher"])
        .args(["-addext", "extendedKeyUsage=codeSigning"])
        .arg("-keyout")
        .arg(&key)
        .arg("-out")
        .arg(&certificate)
        .output()
        .unwrap();
    assert!(made.status.success(), "openssl: {}", String::from_utf8_lossy(&made.stderr));
    Some((certificate, key))
}

/// `osslsigncode` signs into a new file, unlike `signtool`, which signs in
/// place; a two-line shell script makes it behave like `signtool`.
fn osslsigncode_in_place(certificate: &Path, key: &Path, input: &str) -> String {
    format!(
        r#"sh -c "osslsigncode sign -certs {} -key {} -n Test -h sha256 -in {input} -out $1.signed && mv $1.signed $1" sh {{file}}"#,
        certificate.display(),
        key.display(),
    )
}

#[test]
fn an_installer_signed_by_the_publishers_command_is_signed_and_still_installs() {
    let dir = tempfile::tempdir().unwrap();
    let Some((certificate, key)) = signing_tools(dir.path()) else { return };

    let (installer, output) = build(dir.path(), &osslsigncode_in_place(&certificate, &key, "$1"));
    assert!(output.status.success(), "{}", stderr(&output));
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("\"codeSigned\": true"), "{report}");

    // The signature is real: the signing tool's own verifier accepts it.
    let verified = Command::new("osslsigncode")
        .args(["verify", "-CAfile"])
        .arg(&certificate)
        .arg("-in")
        .arg(&installer)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&verified.stdout).contains("Signature verification: ok"),
        "{}",
        String::from_utf8_lossy(&verified.stdout)
    );

    // And the installer still finds, and checks, what it carries.
    let source = xpack_installer::bundle::locate(&installer).unwrap();
    assert!(!xpack_installer::bundle::read(&installer, &source).unwrap().is_empty());
}

#[test]
fn a_command_that_signs_something_else_in_its_place_is_caught() {
    // The installer is replaced by a properly signed file that is not it: the
    // bare stub. Signed, so only the payload check can tell.
    let dir = tempfile::tempdir().unwrap();
    let Some((certificate, key)) = signing_tools(dir.path()) else { return };
    let stub = dir.path().join("stub.exe");

    let command = osslsigncode_in_place(&certificate, &key, &stub.display().to_string());
    let (installer, output) = build(dir.path(), &command);
    assert!(!output.status.success(), "a signed file without its payload was accepted");
    assert!(stderr(&output).contains("cannot read its own payload"), "{}", stderr(&output));
    assert!(!installer.exists(), "the broken installer was left behind");
}

#[test]
fn a_failing_command_fails_the_build_and_leaves_no_installer() {
    let dir = tempfile::tempdir().unwrap();
    // `xpack inspect` refuses an installer: it is not a package.
    let command = format!(r#""{}" inspect {{file}}"#, env!("CARGO_BIN_EXE_xpack"));
    let (installer, output) = build(dir.path(), &command);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("the installer was not signed"), "{}", stderr(&output));
    assert!(!installer.exists(), "an unsigned installer was left where a release would take it");
}

#[test]
fn a_command_that_succeeds_without_signing_is_not_taken_at_its_word() {
    let dir = tempfile::tempdir().unwrap();
    // Exits 0 and touches nothing.
    let command = format!(r#""{}" --version {{file}}"#, env!("CARGO_BIN_EXE_xpack"));
    let (installer, output) = build(dir.path(), &command);
    assert!(!output.status.success(), "an unsigned installer was reported as signed");
    assert!(stderr(&output).contains("carries no Authenticode signature"), "{}", stderr(&output));
    assert!(!installer.exists());
}

#[test]
fn a_program_that_does_not_exist_is_reported_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let (installer, output) = build(dir.path(), "no-such-signing-tool sign {file}");
    assert!(!output.status.success());
    assert!(stderr(&output).contains("could not run no-such-signing-tool"), "{}", stderr(&output));
    assert!(!installer.exists());
}

#[test]
fn a_command_that_never_names_the_installer_is_refused_before_building() {
    let dir = tempfile::tempdir().unwrap();
    let (installer, output) = build(dir.path(), "signtool sign /a");
    assert!(!output.status.success());
    assert!(stderr(&output).contains("{file}"), "{}", stderr(&output));
    assert!(!installer.exists());
}

#[test]
fn signing_is_refused_for_an_installer_that_is_not_for_windows() {
    let dir = tempfile::tempdir().unwrap();
    let package = package(dir.path(), "linux");
    let output = xpack()
        .arg("installer")
        .arg(&package)
        .arg("--stub")
        .arg(stub(dir.path()))
        .arg("--binary")
        .arg(stub(dir.path()))
        .arg("--out")
        .arg(dir.path().join("installer"))
        .args(["--sign-command", "signtool sign {file}"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("signs Windows installers"), "{}", stderr(&output));
}
