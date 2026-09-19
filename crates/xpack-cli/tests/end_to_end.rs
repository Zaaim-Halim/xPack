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

    /// Writes a payload script with the given exit status.
    fn write_payload(&self, version: &str, exit_code: u8) {
        let dir = self.path().join(format!("payload-{version}/bin"));
        std::fs::create_dir_all(&dir).unwrap();
        let script = format!("#!/bin/sh\necho \"running {version} args=$*\"\nexit {exit_code}\n");
        let path = dir.join("app");
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let config = format!(
            r#"{{"application":{{"id":"com.example.demo","name":"Demo","version":"{version}"}},
                "launch":{{"executable":"bin/app"}}}}"#
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
    let first = &parsed.as_array().expect("an array")[0];
    assert_eq!(first["version"], "1.0.0");
    assert_eq!(first["active"], true);
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
