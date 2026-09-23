//! Drives the real `xpack` binary, the way a user would.
//!
//! Unit tests cover the engine. These exist for what only the binary can get
//! wrong: exit codes, which stream output lands on, and whether the commands
//! compose into a workflow that actually works.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Path to the binary under test, provided by cargo.
fn xpack() -> PathBuf {
    // `CARGO_BIN_EXE_<name>` is set for integration tests of a crate with a
    // binary target, so the test always runs the build it belongs to.
    PathBuf::from(env!("CARGO_BIN_EXE_xpack"))
}

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("payload/bin");
        std::fs::create_dir_all(&payload).unwrap();
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("root")
    }

    /// Writes a payload whose application exits with the given status.
    ///
    /// A real executable, not a script. A `#!/bin/sh` payload works on two of
    /// the three platforms xPack supports and fails on Windows with "not a
    /// valid Win32 application", because `CreateProcess` runs executables and
    /// nothing else. The exit status comes from the environment rather than
    /// from an argument, so the user arguments a test passes through the
    /// launcher stay the test's own.
    fn write_payload(&self, version: &str, exit_code: u8) {
        let dir = self.path().join(format!("payload-{version}/bin"));
        std::fs::create_dir_all(&dir).unwrap();

        let name = format!("app{}", std::env::consts::EXE_SUFFIX);
        std::fs::copy(env!("CARGO_BIN_EXE_xpack-test-payload"), dir.join(&name)).unwrap();

        let config = format!(
            r#"{{"application":{{"id":"com.example.demo","name":"Demo","version":"{version}"}},
                "launch":{{"executable":"bin/{name}",
                           "environment":{{"XPACK_TEST_PAYLOAD_EXIT":"{exit_code}",
                                          "XPACK_TEST_PAYLOAD_VERSION":"{version}"}}}}}}"#
        );
        std::fs::write(self.path().join(format!("xpack-{version}.json")), config).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(xpack())
            .current_dir(self.path())
            .args(args)
            .output()
            .expect("the xpack binary should run")
    }

    /// Runs a command scoped to this fixture's installation root.
    fn run_in_root(&self, args: &[&str]) -> Output {
        let root = self.root();
        let mut full: Vec<String> = vec!["--root".into(), root.to_string_lossy().into_owned()];
        full.extend(args.iter().map(|a| (*a).to_string()));
        Command::new(xpack())
            .current_dir(self.path())
            .args(&full)
            .output()
            .expect("the xpack binary should run")
    }

    fn keygen(&self) {
        let out = self.run(&["keygen", "--out", "signing.json"]);
        assert!(out.status.success(), "keygen failed: {}", stderr(&out));
    }

    fn pack(&self, version: &str) -> String {
        self.write_payload(version, 0);
        self.pack_existing(version)
    }

    fn pack_existing(&self, version: &str) -> String {
        let out = self.run(&[
            "pack",
            &format!("payload-{version}"),
            "--config",
            &format!("xpack-{version}.json"),
            "--key",
            "signing.json",
            "--out",
            &format!("demo-{version}.xpkg"),
        ]);
        assert!(out.status.success(), "pack failed: {}", stderr(&out));
        format!("demo-{version}.xpkg")
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap_or(-1)
}

#[test]
fn the_full_lifecycle_works_from_the_command_line() {
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");

    let install = fixture.run_in_root(&["install", &package, "--trust", "signing.pub.json"]);
    assert!(install.status.success(), "{}", stderr(&install));
    assert!(stdout(&install).contains("1.0.0"));

    let run = fixture.run_in_root(&["run", "com.example.demo", "--", "--flag"]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert!(
        stdout(&run).contains("running 1.0.0 args=--flag"),
        "user arguments must reach the application: {}",
        stdout(&run)
    );

    let list = fixture.run_in_root(&["list", "com.example.demo"]);
    assert!(stdout(&list).contains("1.0.0"));
}

#[test]
fn a_failing_update_rolls_back_automatically() {
    let fixture = Fixture::new();
    fixture.keygen();
    let good = fixture.pack("1.0.0");
    fixture.run_in_root(&["install", &good, "--trust", "signing.pub.json"]);
    fixture.run_in_root(&["run", "com.example.demo"]);

    // 1.1.0 exits non-zero, so probation must fail and roll back.
    fixture.write_payload("1.1.0", 7);
    let broken = fixture.pack_existing("1.1.0");
    fixture.run_in_root(&["install", &broken]);

    let run = fixture.run_in_root(&["run", "com.example.demo"]);
    assert_eq!(code(&run), 7, "the application's own exit status must pass through");
    assert!(stderr(&run).contains("rolled back to 1.0.0"), "{}", stderr(&run));

    // The next run must land on the healthy version.
    let again = fixture.run_in_root(&["run", "com.example.demo"]);
    assert!(again.status.success(), "{}", stderr(&again));
    assert!(stdout(&again).contains("running 1.0.0"));
}

#[test]
fn security_failures_use_a_distinct_exit_code() {
    // A script retrying on failure must never retry a signature mismatch.
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");

    let untrusted = fixture.run_in_root(&["install", &package]);
    assert_eq!(code(&untrusted), 3, "an untrusted install is a security failure");

    let low_order = fixture.run(&[
        "verify",
        &package,
        "--key",
        "0000000000000000000000000000000000000000000000000000000000000000",
    ]);
    assert_eq!(code(&low_order), 3, "a low-order key is a security failure");

    let typo = fixture.run(&["verify", &package, "--key", "not-a-key"]);
    assert_eq!(code(&typo), 1, "a malformed key is a typo, not an attack");
}

#[test]
fn results_go_to_stdout_and_diagnostics_to_stderr() {
    // Output must stay pipeable, so nothing informational may pollute stdout.
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");

    let inspect = fixture.run(&["inspect", &package]);
    assert!(stdout(&inspect).contains("com.example.demo"), "data belongs on stdout");
    assert!(
        stderr(&inspect).contains("warning") && stderr(&inspect).contains("verified"),
        "the unverified warning belongs on stderr: {}",
        stderr(&inspect)
    );
    assert!(
        !stdout(&inspect).contains("warning"),
        "warnings must not appear on stdout: {}",
        stdout(&inspect)
    );
}

#[test]
fn list_emits_parseable_json() {
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");
    fixture.run_in_root(&["install", &package, "--trust", "signing.pub.json"]);

    let out = fixture.run_in_root(&["list", "com.example.demo", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout must be valid JSON ({e}): {}", stdout(&out)));

    let first = &parsed["versions"].as_array().expect("an array of versions")[0];
    assert_eq!(first["version"], "1.0.0");
    assert_eq!(first["active"], true);

    // The executables are named after the application, so a caller that
    // cannot read their paths from here has no way to find them but to
    // reimplement the naming rule.
    assert_eq!(parsed["application"], "com.example.demo");

    // `root` and `launcher` are always there: an installation has a
    // directory, and one is installed unless `--no-launcher` said otherwise.
    for field in ["root", "launcher"] {
        let path = parsed[field].as_str().unwrap_or_else(|| panic!("{field} is missing"));
        assert!(
            std::path::Path::new(path).exists(),
            "{field} names something that is not there: {path}"
        );
    }

    // The rest depend on which binaries were beside the one that installed
    // them, so what is asserted is the invariant rather than their presence:
    // a path is reported only when a file is actually at it.
    for field in ["consoleLauncher", "updater", "uninstaller"] {
        if let Some(path) = parsed[field].as_str() {
            assert!(
                std::path::Path::new(path).exists(),
                "{field} names something that is not there: {path}"
            );
        }
    }

    // And what it reports is the launcher a user would actually run.
    let launcher = parsed["launcher"].as_str().unwrap();
    let ran = Command::new(launcher).output().expect("the reported launcher should run");
    assert!(ran.status.success(), "the reported launcher failed: {}", stderr(&ran));
}

#[test]
fn listing_an_installation_with_no_executables_reports_no_paths() {
    // Reporting a path that is not there would hand a caller something to run
    // that does not exist, which is worse than saying nothing.
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");
    fixture.run_in_root(&["install", &package, "--trust", "signing.pub.json", "--no-launcher"]);

    let out = fixture.run_in_root(&["list", "com.example.demo", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();

    for field in ["launcher", "consoleLauncher", "updater", "uninstaller"] {
        assert!(parsed[field].is_null(), "{field} should be null: {}", parsed[field]);
    }
    assert_eq!(parsed["versions"].as_array().unwrap().len(), 1);
}

#[test]
fn trust_on_first_use_pins_and_then_refuses_another_publisher() {
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");

    let first = fixture.run_in_root(&["install", &package, "--trust-on-first-use"]);
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(
        stderr(&first).contains("trusted on sight"),
        "the weaker guarantee must be stated: {}",
        stderr(&first)
    );

    // A different publisher signs 2.0.0; first use has passed, so it is refused.
    let out = fixture.run(&["keygen", "--out", "attacker.json", "--force"]);
    assert!(out.status.success());
    fixture.write_payload("2.0.0", 0);
    let forged = fixture.run(&[
        "pack",
        "payload-2.0.0",
        "--config",
        "xpack-2.0.0.json",
        "--key",
        "attacker.json",
        "--out",
        "forged.xpkg",
    ]);
    assert!(forged.status.success(), "{}", stderr(&forged));

    let attacked = fixture.run_in_root(&["install", "forged.xpkg", "--trust-on-first-use"]);
    assert_eq!(code(&attacked), 3, "a second publisher must be refused");
}

#[test]
fn overwriting_a_signing_key_requires_force() {
    // Overwriting makes every existing installation unable to accept updates.
    let fixture = Fixture::new();
    fixture.keygen();
    let again = fixture.run(&["keygen", "--out", "signing.json"]);
    assert!(!again.status.success());
    assert!(stderr(&again).contains("--force"), "{}", stderr(&again));
}

#[test]
fn uninstall_requires_confirmation() {
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");
    fixture.run_in_root(&["install", &package, "--trust", "signing.pub.json"]);

    let refused = fixture.run_in_root(&["uninstall", "com.example.demo"]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("--yes"));

    let done = fixture.run_in_root(&["uninstall", "com.example.demo", "--yes"]);
    assert!(done.status.success(), "{}", stderr(&done));
}

#[test]
fn an_unparseable_command_line_exits_two() {
    let fixture = Fixture::new();
    assert_eq!(code(&fixture.run(&["no-such-command"])), 2);
}

#[test]
fn installing_a_downgrade_is_refused_until_asked_for() {
    let fixture = Fixture::new();
    fixture.keygen();
    let newer = fixture.pack("1.1.0");
    fixture.run_in_root(&["install", &newer, "--trust", "signing.pub.json"]);

    let older = fixture.pack("1.0.0");
    let refused = fixture.run_in_root(&["install", &older]);
    assert!(!refused.status.success(), "{}", stdout(&refused));

    let allowed = fixture.run_in_root(&["install", &older, "--allow-downgrade"]);
    assert!(allowed.status.success(), "{}", stderr(&allowed));
}

/// The directory the test payload reported starting in.
fn reported_cwd(run: &Output) -> PathBuf {
    let out = stdout(run);
    let line = out.lines().find_map(|l| l.strip_prefix("cwd=")).expect("the payload reports cwd");
    std::fs::canonicalize(line).unwrap()
}

#[test]
fn a_command_line_tool_runs_where_the_user_invoked_it() {
    // A tool the user types `mytool build src` into must see `src` relative
    // to where they are, not to wherever it happens to be installed.
    let fixture = Fixture::new();
    fixture.keygen();
    fixture.write_payload("1.0.0", 0);
    let config = fixture.path().join("xpack-1.0.0.json");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        &config,
        text.replace(
            r#""launch":{"#,
            r#""launch":{"keepWorkingDirectory":true,"arguments":["{versionDir}/data"],"#,
        ),
    )
    .unwrap();
    let package = fixture.pack_existing("1.0.0");

    let install = fixture.run_in_root(&["install", &package, "--trust", "signing.pub.json"]);
    assert!(install.status.success(), "{}", stderr(&install));

    // The package declares the format that can express it, and no lower.
    let manifest = fixture.root().join("com.example.demo/versions/1.0.0/.xpack/manifest.json");
    let manifest = std::fs::read_to_string(&manifest).unwrap();
    assert!(manifest.contains("\"formatVersion\": 2"), "{manifest}");

    let elsewhere = fixture.path().join("where-the-user-is");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let run = Command::new(xpack())
        .current_dir(&elsewhere)
        .args(["--root", &fixture.root().to_string_lossy(), "run", "com.example.demo"])
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(reported_cwd(&run), std::fs::canonicalize(&elsewhere).unwrap());

    // Started somewhere else, it still finds its own files: the argument
    // naming one arrives as the version directory's absolute path. Joined a
    // component at a time, so the separators are the platform's own, as the
    // launcher's are: one `join` of "a/b/c" keeps the `/` on Windows.
    let version_dir = fixture.root().join("com.example.demo").join("versions").join("1.0.0");
    let expected = format!("args={}/data", version_dir.display());
    assert!(stdout(&run).contains(&expected), "{}", stdout(&run));
}

#[test]
fn an_application_that_does_not_ask_still_starts_in_its_version_directory() {
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");
    let install = fixture.run_in_root(&["install", &package, "--trust", "signing.pub.json"]);
    assert!(install.status.success(), "{}", stderr(&install));

    // Format 1 still: nothing in it needs more, so every earlier release can
    // install it.
    let version_dir = fixture.root().join("com.example.demo/versions/1.0.0");
    let manifest = std::fs::read_to_string(version_dir.join(".xpack/manifest.json")).unwrap();
    assert!(manifest.contains("\"formatVersion\": 1"), "{manifest}");

    let run = fixture.run_in_root(&["run", "com.example.demo"]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(reported_cwd(&run), std::fs::canonicalize(&version_dir).unwrap());
}

/// Returns the distinct ANSI escape sequences in a byte stream.
///
/// Looks for the CSI introducer rather than colour specifically: any escape at
/// all in a captured stream is output written for a terminal that is not there.
fn escape_sequences(bytes: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b'[' {
            let start = i;
            i += 2;
            while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            let sequence = String::from_utf8_lossy(&bytes[start..=i.min(bytes.len() - 1)])
                .replace('\x1b', "ESC");
            if !found.contains(&sequence) {
                found.push(sequence);
            }
        }
        i += 1;
    }
    found
}

#[test]
fn output_carries_no_terminal_escapes_when_nothing_is_watching() {
    // xPack runs headless far more often than it runs in front of someone: on
    // servers, in containers, over SSH, under a scheduler, piped into a log.
    // `Command::output` captures through pipes, so this runs under exactly the
    // condition that matters — no terminal on either stream.
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");

    let install = fixture.run_in_root(&["install", &package, "--trust", "signing.pub.json"]);
    assert!(install.status.success(), "{}", stderr(&install));

    for (stream, bytes) in [("stdout", &install.stdout), ("stderr", &install.stderr)] {
        let escapes = escape_sequences(bytes);
        assert!(
            escapes.is_empty(),
            "install wrote terminal escapes to {stream}: {escapes:?}\n\
             full output:\n{}",
            String::from_utf8_lossy(bytes)
        );
    }

    // The same for a command that reads state and prints a table, where
    // highlighting the active version is most tempting.
    let list = fixture.run_in_root(&["list", "com.example.demo"]);
    for (stream, bytes) in [("stdout", &list.stdout), ("stderr", &list.stderr)] {
        let escapes = escape_sequences(bytes);
        assert!(escapes.is_empty(), "list wrote terminal escapes to {stream}: {escapes:?}");
    }
}

#[test]
fn no_command_waits_for_input_that_will_never_come() {
    // A prompt in a headless run is a hang, and a hang under a scheduler is an
    // outage. Destructive commands take a flag instead; stdin is closed here to
    // prove nothing reads it.
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");
    fixture.run_in_root(&["install", &package, "--trust", "signing.pub.json"]);

    let refused = Command::new(xpack())
        .current_dir(fixture.path())
        .args(["--root", &fixture.root().to_string_lossy(), "uninstall", "com.example.demo"])
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the xpack binary should run");

    // Refused for want of a flag, not blocked waiting for someone to type.
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("--yes"),
        "expected a flag to be demanded, got: {}",
        stderr(&refused)
    );
}

// --- publishing: `pack --json` and `index` -------------------------------
//
// These are the commands a build-system integration drives. What they must
// get right is machine-readable output and an index the *updater* can parse —
// so these tests assert on parsed JSON, never on prose.

#[test]
fn pack_json_reports_everything_needed_to_publish() {
    let fixture = Fixture::new();
    fixture.keygen();
    fixture.write_payload("1.0.0", 0);

    let out = fixture.run(&[
        "pack",
        "payload-1.0.0",
        "--config",
        "xpack-1.0.0.json",
        "--key",
        "signing.json",
        "--out",
        "demo-1.0.0.xpkg",
        "--json",
    ]);
    assert!(out.status.success(), "pack --json failed: {}", stderr(&out));

    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout should be JSON");

    assert_eq!(report["application"], "com.example.demo");
    assert_eq!(report["version"], "1.0.0");
    // The two values an update index cannot be built without.
    assert!(report["sha256"].as_str().is_some_and(|s| s.len() == 64), "{report}");
    assert!(report["size"].as_u64().is_some_and(|n| n > 0), "{report}");
    assert!(report["signedBy"].as_str().is_some(), "{report}");

    // The platform must come back in the form `--platform` accepts. A nested
    // {os, arch} object — which is how a Platform serialises into a manifest —
    // would make every integration reassemble the string itself.
    let platform = report["platform"].as_str().expect("platform should be a string");
    assert!(platform.contains('-'), "expected <os>-<arch>, got {platform:?}");

    let again = fixture.run(&[
        "pack",
        "payload-1.0.0",
        "--config",
        "xpack-1.0.0.json",
        "--key",
        "signing.json",
        "--out",
        "round-trip.xpkg",
        "--platform",
        platform,
        "--json",
    ]);
    assert!(
        again.status.success(),
        "the reported platform must be accepted back: {}",
        stderr(&again)
    );
}

#[test]
fn index_writes_a_document_the_updater_can_parse() {
    // The whole point of the command. If the CLI and the updater disagree
    // about this format, every release is broken and nothing else catches it.
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");

    let out = fixture.run(&["index", &package, "--out-dir", "updates", "--json"]);
    assert!(out.status.success(), "index failed: {}", stderr(&out));

    let written: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout should be JSON");
    let entry = &written[0];
    let index_path = entry["path"].as_str().expect("an index path");

    let body = std::fs::read(fixture.path().join(index_path)).expect("the index should exist");

    // Parsed by the updater's own type, not by a hand-written shape here.
    let index = xpack_update::index::UpdateIndex::from_slice(&body).expect("updater must parse it");
    assert_eq!(index.application, "com.example.demo");
    assert_eq!(index.version.to_string(), "1.0.0");
    assert_eq!(index.package.file, package);
    assert!(index.package.size > 0);
    assert!(index.package.sha256.is_some(), "a digest is what bounds the download");
}

#[test]
fn the_index_digest_matches_the_package_on_disk() {
    // A digest that does not match is worse than none: every client downloads
    // the package and then rejects it.
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");

    let packed = fixture.run(&[
        "pack",
        "payload-1.0.0",
        "--config",
        "xpack-1.0.0.json",
        "--key",
        "signing.json",
        "--out",
        "recomputed.xpkg",
        "--json",
    ]);
    assert!(packed.status.success(), "{}", stderr(&packed));

    let out = fixture.run(&["index", &package, "--out-dir", "updates", "--json"]);
    assert!(out.status.success(), "index failed: {}", stderr(&out));
    let written: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let index_path = written[0]["path"].as_str().unwrap();

    let body = std::fs::read(fixture.path().join(index_path)).unwrap();
    let index = xpack_update::index::UpdateIndex::from_slice(&body).unwrap();

    // Recompute independently of both commands.
    let bytes = std::fs::read(fixture.path().join(&package)).unwrap();
    let expected = xpack_security::sha256(&bytes);
    assert_eq!(index.package.sha256.unwrap(), expected);
    assert_eq!(index.package.size, bytes.len() as u64);
}

#[test]
fn index_lands_where_the_updater_would_fetch_it() {
    // `<update.url>/<channel>.json`, mirrored on disk as
    // `<out-dir>/<platform>/<channel>.json`, so uploading the tree works.
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");

    let out = fixture.run(&["index", &package, "--out-dir", "updates"]);
    assert!(out.status.success(), "index failed: {}", stderr(&out));

    let platform = xpack_core::Platform::host().unwrap().to_string();
    let expected = fixture.path().join("updates").join(&platform).join("stable.json");
    assert!(expected.is_file(), "expected an index at {}", expected.display());
}

#[test]
fn index_verifies_when_given_a_key() {
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");

    let out =
        fixture.run(&["index", &package, "--out-dir", "updates", "--key", "signing.pub.json"]);
    assert!(out.status.success(), "index --key failed: {}", stderr(&out));
    // With a key supplied, the "not verified" warning must not appear.
    assert!(!stderr(&out).contains("not verified"), "{}", stderr(&out));
}

#[test]
fn index_refuses_a_package_signed_by_another_key() {
    // An index built from a package that does not verify is a release every
    // client downloads and refuses.
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");

    let other = fixture.run(&["keygen", "--out", "other.json", "--force"]);
    assert!(other.status.success(), "{}", stderr(&other));

    let out = fixture.run(&["index", &package, "--out-dir", "updates", "--key", "other.pub.json"]);
    assert!(!out.status.success(), "a mismatched key must fail");
    assert_eq!(out.status.code(), Some(3), "a signature failure has its own exit code");
}

#[test]
fn index_refuses_two_applications_in_one_run() {
    let fixture = Fixture::new();
    fixture.keygen();
    let first = fixture.pack("1.0.0");

    // A second package with a different application id.
    std::fs::create_dir_all(fixture.path().join("payload-other/bin")).unwrap();
    std::fs::write(fixture.path().join("payload-other/bin/app"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(
        fixture.path().join("xpack-other.json"),
        r#"{"application":{"id":"com.example.other","name":"Other","version":"1.0.0"},
            "launch":{"executable":"bin/app"}}"#,
    )
    .unwrap();
    let packed = fixture.run(&[
        "pack",
        "payload-other",
        "--config",
        "xpack-other.json",
        "--key",
        "signing.json",
        "--out",
        "other-1.0.0.xpkg",
    ]);
    assert!(packed.status.success(), "{}", stderr(&packed));

    let out = fixture.run(&["index", &first, "other-1.0.0.xpkg", "--out-dir", "updates"]);
    assert!(!out.status.success(), "a mixed set must be refused");
    assert!(stderr(&out).contains("more than one application"), "{}", stderr(&out));
}

// --- key material must never be published --------------------------------

#[test]
fn packing_a_directory_containing_the_signing_key_is_refused() {
    // The accident this prevents: `xpack keygen` writes into the working
    // directory and `xpack pack .` packages the working directory, so the two
    // defaults compose into publishing the key that signs every future update.
    let fixture = Fixture::new();
    fixture.write_payload("1.0.0", 0);

    // Generate the key *inside* the payload, which is what happens when
    // someone runs both commands in the same folder.
    let payload = "payload-1.0.0".to_string();
    let key_in_payload = format!("{payload}/xpack-signing.json");
    let out = fixture.run(&["keygen", "--out", &key_in_payload]);
    assert!(out.status.success(), "keygen failed: {}", stderr(&out));

    let out = fixture.run(&[
        "pack",
        &payload,
        "--config",
        "xpack-1.0.0.json",
        "--key",
        &key_in_payload,
        "--out",
        "leak.xpkg",
    ]);

    assert!(!out.status.success(), "packaging the signing key must be refused");
    assert!(
        !fixture.path().join("leak.xpkg").exists(),
        "a package was written despite the refusal"
    );
}

#[test]
fn a_stray_private_key_anywhere_in_the_payload_is_refused() {
    // Not the key being used to sign — an old one left in the tree. The
    // library check is on content, so it is caught wherever it sits.
    let fixture = Fixture::new();
    fixture.keygen();
    fixture.write_payload("1.0.0", 0);

    let stray = "payload-1.0.0/bin/old-signing.json";
    let out = fixture.run(&["keygen", "--out", stray]);
    assert!(out.status.success(), "keygen failed: {}", stderr(&out));

    let out = fixture.run(&[
        "pack",
        "payload-1.0.0",
        "--config",
        "xpack-1.0.0.json",
        "--key",
        "signing.json",
        "--out",
        "stray.xpkg",
    ]);

    assert!(!out.status.success(), "a stray private key must be refused");
    assert!(stderr(&out).contains("private signing key"), "{}", stderr(&out));
}

// --- the bootstrap installer ---------------------------------------------

/// The directory cargo put this test's binaries in.
fn binary_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_xpack")).parent().unwrap().to_path_buf()
}

/// The `xpack-installer` stub built alongside this test.
fn installer_stub() -> PathBuf {
    binary_dir().join(format!("xpack-installer{}", std::env::consts::EXE_SUFFIX))
}

/// Whether every binary `xpack installer` needs has been built.
///
/// `cargo test -p xpack-cli` builds this crate's binary and its tests, but not
/// the *other* crates' binaries, and `xpack installer` gathers the launcher,
/// updater and uninstaller from beside itself. Checking only the stub made
/// these tests fail rather than skip whenever the workspace had not been built
/// — a failure that says nothing about the code under test.
///
/// CI runs `cargo build --workspace` first, so there they always run.
fn installer_binaries_are_built() -> bool {
    ["xpack-installer", "xpack-launcher", "xpack-updater", "xpack-uninstaller"]
        .iter()
        .all(|name| binary_dir().join(format!("{name}{}", std::env::consts::EXE_SUFFIX)).is_file())
}

#[test]
fn an_installer_installs_an_application_that_then_runs() {
    // The whole point of the installer, asserted by running it and then
    // running what it installed. Everything else about this crate can be
    // right and this still be broken.
    if !installer_binaries_are_built() {
        eprintln!(
            "skipping: run `cargo build --workspace` first; see installer_binaries_are_built"
        );
        return;
    }

    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");

    let built = fixture.run(&[
        "installer",
        &package,
        "--out",
        "Demo-installer",
        "--stub",
        &installer_stub().to_string_lossy(),
        "--json",
    ]);
    assert!(built.status.success(), "building the installer failed: {}", stderr(&built));

    let report: serde_json::Value = serde_json::from_slice(&built.stdout).unwrap();
    assert_eq!(report["application"], "com.example.demo");
    assert!(report["size"].as_u64().is_some_and(|n| n > 0));

    // Run it. The layout differs per platform, so ask the report where the
    // executable is rather than assuming.
    let installer = match report["layout"].as_str() {
        Some("bundle") => {
            let mut found = None;
            let macos = fixture.path().join("Demo-installer/Contents/MacOS");
            for entry in std::fs::read_dir(&macos).unwrap() {
                found = Some(entry.unwrap().path());
            }
            found.expect("a bundle executable")
        }
        _ => fixture.path().join("Demo-installer"),
    };

    let root = fixture.path().join("installed");
    // `--silent` so this can never wait on a window: builds that open a
    // wizard do so only when run without it.
    let ran = Command::new(&installer)
        .current_dir(fixture.path())
        .args(["--root", &root.to_string_lossy(), "--silent"])
        .output()
        .expect("the installer should run");
    assert!(ran.status.success(), "the installer failed: {}", stderr(&ran));

    // Asking again says what installing again would do, with its exit code,
    // and changes nothing.
    let asked = Command::new(&installer)
        .args(["--root", &root.to_string_lossy(), "--silent", "--dry-run"])
        .output()
        .expect("the installer should run");
    assert_eq!(asked.status.code(), Some(1), "{}", stdout(&asked));
    assert!(stdout(&asked).contains("already installed"), "{}", stdout(&asked));

    // And now the application itself. Which launcher that is differs by
    // platform — Windows installs a windowed build as well as a console one
    // — so the installation is asked rather than the path reconstructed,
    // which is the same thing any other tool integrating with xPack has to
    // do.
    let listed = Command::new(xpack())
        .args(["--root", &root.to_string_lossy()])
        .args(["list", "com.example.demo", "--json"])
        .output()
        .expect("listing the installation should work");
    let listing: serde_json::Value = serde_json::from_slice(&listed.stdout)
        .unwrap_or_else(|e| panic!("list --json must parse ({e}): {}", stdout(&listed)));
    let launcher =
        PathBuf::from(listing["launcher"].as_str().expect("the installation reports a launcher"));
    assert!(launcher.is_file(), "no launcher was installed at {}", launcher.display());

    let app =
        Command::new(&launcher).arg("hello").output().expect("the installed launcher should run");
    assert!(app.status.success(), "the installed application failed: {}", stderr(&app));
    assert!(
        String::from_utf8_lossy(&app.stdout).contains("args=hello"),
        "the application did not receive its arguments: {}",
        String::from_utf8_lossy(&app.stdout)
    );
}

#[test]
fn a_dry_run_installs_nothing() {
    if !installer_binaries_are_built() {
        eprintln!(
            "skipping: run `cargo build --workspace` first; see installer_binaries_are_built"
        );
        return;
    }

    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");

    let built = fixture.run(&[
        "installer",
        &package,
        "--out",
        "Demo-installer",
        "--stub",
        &installer_stub().to_string_lossy(),
    ]);
    assert!(built.status.success(), "{}", stderr(&built));

    let installer = if fixture.path().join("Demo-installer/Contents").is_dir() {
        std::fs::read_dir(fixture.path().join("Demo-installer/Contents/MacOS"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
    } else {
        fixture.path().join("Demo-installer")
    };

    let root = fixture.path().join("installed");
    let ran = Command::new(&installer)
        .args(["--root", &root.to_string_lossy(), "--silent", "--dry-run"])
        .output()
        .expect("the installer should run");

    assert!(ran.status.success(), "{}", stderr(&ran));
    assert!(!root.exists(), "a dry run created {}", root.display());
}

#[test]
fn an_installer_carries_the_wizard_settings_and_the_packages_icon() {
    if !installer_binaries_are_built() {
        eprintln!(
            "skipping: run `cargo build --workspace` first; see installer_binaries_are_built"
        );
        return;
    }

    let fixture = Fixture::new();
    fixture.keygen();
    fixture.write_payload("1.0.0", 0);

    // The icon is named once, in the package, and every surface takes it
    // from there.
    let icon: &[u8] = b"not a real icns, but the bytes must arrive intact";
    std::fs::write(fixture.path().join("payload-1.0.0/icon.icns"), icon).unwrap();
    let config_path = fixture.path().join("xpack-1.0.0.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["desktop"] = serde_json::json!({ "icon": "icon.icns" });
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let package = fixture.pack_existing("1.0.0");

    std::fs::write(fixture.path().join("LICENSE.txt"), "Terms of use.").unwrap();
    std::fs::write(fixture.path().join("ui.json"), r#"{"license": "LICENSE.txt"}"#).unwrap();

    let built = fixture.run(&[
        "installer",
        &package,
        "--out",
        "Demo-installer",
        "--stub",
        &installer_stub().to_string_lossy(),
        "--ui",
        "ui.json",
    ]);
    assert!(built.status.success(), "building the installer failed: {}", stderr(&built));

    let bundle = fixture.path().join("Demo-installer/Contents");
    let installer = if bundle.is_dir() {
        // macOS: Finder shows the package's icon on the installer itself.
        assert_eq!(std::fs::read(bundle.join("Resources/AppIcon.icns")).unwrap(), icon);
        let plist = std::fs::read_to_string(bundle.join("Info.plist")).unwrap();
        assert!(plist.contains("CFBundleIconFile"), "{plist}");
        std::fs::read_dir(bundle.join("MacOS")).unwrap().next().unwrap().unwrap().path()
    } else {
        fixture.path().join("Demo-installer")
    };

    // The settings change nothing for a run without a window.
    let root = fixture.path().join("installed");
    let ran = Command::new(&installer)
        .args(["--root", &root.to_string_lossy(), "--silent", "--dry-run"])
        .output()
        .expect("the installer should run");
    assert!(ran.status.success(), "{}", stderr(&ran));

    // And a settings file that is wrong stops the build rather than shipping
    // a default nobody asked for.
    std::fs::write(fixture.path().join("bad.json"), r#"{"launchOnFinnish": true}"#).unwrap();
    let refused = fixture.run(&[
        "installer",
        &package,
        "--out",
        "Refused-installer",
        "--stub",
        &installer_stub().to_string_lossy(),
        "--ui",
        "bad.json",
    ]);
    assert!(!refused.status.success(), "a misspelt setting was accepted");
}

/// A stream whose reader has gone away: every write to it fails.
fn closed_pipe() -> std::process::Stdio {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    writer.into()
}

/// Runs a command scoped to this fixture's root with both streams unread.
fn run_unread(fixture: &Fixture, args: &[&str]) -> std::process::ExitStatus {
    Command::new(xpack())
        .current_dir(fixture.path())
        .arg("--root")
        .arg(fixture.root())
        .args(args)
        .stdout(closed_pipe())
        .stderr(closed_pipe())
        .status()
        .expect("the xpack binary should run")
}

#[test]
fn an_install_nobody_reads_about_still_happens() {
    // `xpack install … | head -1` in a script, or a CI step whose log
    // collector died: the output is lost, the install must not be.
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");

    let status = run_unread(&fixture, &["install", &package, "--trust", "signing.pub.json"]);
    assert_eq!(status.code(), Some(0), "{status:?}");

    let list = fixture.run_in_root(&["list", "com.example.demo"]);
    assert!(stdout(&list).contains("1.0.0"), "not installed: {}", stdout(&list));
}

#[test]
fn a_report_piped_into_something_that_stops_reading_is_not_a_crash() {
    // `xpack list --json | head -c 1`: the reader has what it wanted.
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");
    let install = fixture.run_in_root(&["install", &package, "--trust", "signing.pub.json"]);
    assert!(install.status.success(), "{}", stderr(&install));

    for args in [
        &["list", "com.example.demo", "--json"][..],
        &["list", "com.example.demo"][..],
        &["inspect", &package][..],
    ] {
        let status = run_unread(&fixture, args);
        assert_eq!(status.code(), Some(0), "xpack {args:?}: {status:?}");
    }
}

#[test]
fn a_failure_nobody_can_read_keeps_its_exit_code() {
    // The code is the only report left, so it must be the failure's own:
    // 3 for a package that does not verify, not the 101 of a crash.
    let fixture = Fixture::new();
    fixture.keygen();
    let package = fixture.pack("1.0.0");

    let status = run_unread(&fixture, &["install", &package]);
    assert_eq!(status.code(), Some(3), "{status:?}");
}
