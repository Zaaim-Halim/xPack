//! Keeps `docs/README.md` honest about the command line.
//!
//! A document describing a command line drifts from it the moment someone adds
//! a flag, and nothing about a passing build notices. These tests compare the
//! document against the binaries themselves.
//!
//! # Why this is a test and not a CI job
//!
//! `docs/` is deliberately untracked, so a CI checkout does not contain the
//! file. A workflow step reading it would either fail on every run or — worse —
//! skip on every run and look like coverage while checking nothing.
//!
//! So the check lives where the file does: on the machine of whoever is
//! editing the CLI. That is where the drift is introduced, and the last place
//! it can be fixed before it ships. When the document is absent these tests
//! skip, and say why.
//!
//! # What is compared, and what is not
//!
//! Commands are compared **both** ways: one missing from the document means an
//! undocumented feature, one that lingers after a command is removed means a
//! reader following instructions that no longer work.
//!
//! Flags are compared against the document **as a whole** rather than
//! per-command. Prose groups commands together in examples, so attributing a
//! flag in a shared code block to one command produces false failures — which
//! is how a check like this comes to be ignored, and then deleted.
//!
//! Descriptions and help text are not compared at all. They are prose, they
//! should differ, and demanding they match would make the document a
//! transcript of `--help` instead of something worth reading.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Flags every command inherits. Documented once, not per command.
const GLOBAL_FLAGS: [&str; 4] = ["--root", "--verbose", "--help", "--version"];

/// The binary under test.
fn xpack() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_xpack"))
}

/// The document, if this checkout has one.
///
/// Returns `None` in CI and in any fresh clone, because `docs/` is untracked.
fn document() -> Option<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/README.md");
    std::fs::read_to_string(path).ok()
}

/// Announces a skip so a silent pass is never mistaken for a check.
fn skipped() {
    eprintln!(
        "skipping: docs/README.md is not in this checkout, which is expected in CI — \
         `docs/` is untracked"
    );
}

/// Every subcommand the CLI offers, `help` aside.
fn commands() -> BTreeSet<String> {
    let output = Command::new(xpack()).arg("--help").output().expect("xpack --help should run");
    let text = String::from_utf8_lossy(&output.stdout);

    let listing = text
        .split_once("Commands:")
        .and_then(|(_, rest)| rest.split_once("\nOptions:").map(|(c, _)| c))
        .expect("`--help` should list commands then options");

    listing
        .lines()
        .filter_map(|line| {
            let trimmed = line.strip_prefix("  ")?;
            let name = trimmed.split_whitespace().next()?;
            (name != "help" && !name.starts_with('-')).then(|| name.to_string())
        })
        .collect()
}

/// Long flags a binary accepts, excluding the global ones.
fn flags_of(binary: &Path, subcommand: Option<&str>) -> BTreeSet<String> {
    let mut command = Command::new(binary);
    if let Some(subcommand) = subcommand {
        command.arg(subcommand);
    }
    let output = command.arg("--help").output().expect("--help should run");
    let text = String::from_utf8_lossy(&output.stdout);

    // Only the options section: a usage line or a description can mention a
    // flag belonging to something else.
    let options = text.split_once("Options:").map_or(String::new(), |(_, rest)| rest.to_string());

    options
        .lines()
        .filter_map(|line| {
            let start = line.find("--")?;
            let flag: String =
                line[start..].chars().take_while(|c| c.is_ascii_lowercase() || *c == '-').collect();
            (flag.len() > 2 && !GLOBAL_FLAGS.contains(&flag.as_str())).then_some(flag)
        })
        .collect()
}

/// Commands named in the document's own command table.
fn documented_commands(doc: &str) -> BTreeSet<String> {
    doc.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("| `xpack ")?;
            let (name, _) = rest.split_once('`')?;
            (!name.contains(' ')).then(|| name.to_string())
        })
        .collect()
}

#[test]
fn every_command_is_documented() {
    let Some(doc) = document() else {
        return skipped();
    };

    let missing: Vec<String> = commands().difference(&documented_commands(&doc)).cloned().collect();

    assert!(
        missing.is_empty(),
        "these commands exist but are not in the command table in docs/README.md §13.1: {missing:?}"
    );
}

#[test]
fn no_documented_command_has_been_removed() {
    // The other direction. A reader following instructions for a command that
    // no longer exists is worse served than one who never found it.
    let Some(doc) = document() else {
        return skipped();
    };

    let stale: Vec<String> = documented_commands(&doc).difference(&commands()).cloned().collect();

    assert!(stale.is_empty(), "docs/README.md documents commands that no longer exist: {stale:?}");
}

#[test]
fn every_flag_appears_somewhere_in_the_document() {
    let Some(doc) = document() else {
        return skipped();
    };

    let mut undocumented = Vec::new();
    for command in commands() {
        for flag in flags_of(&xpack(), Some(&command)) {
            if !doc.contains(&flag) {
                undocumented.push(format!("xpack {command} {flag}"));
            }
        }
    }

    assert!(
        undocumented.is_empty(),
        "these flags exist but appear nowhere in docs/README.md: {undocumented:#?}"
    );
}

#[test]
fn every_shipped_binarys_flags_are_documented() {
    // The launcher takes no flags of its own by design, so it is not listed.
    let Some(doc) = document() else {
        return skipped();
    };

    let directory = xpack().parent().expect("a build directory").to_path_buf();
    let mut undocumented = Vec::new();

    for name in ["xpack-installer", "xpack-updater", "xpack-uninstaller"] {
        let binary = directory.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        if !binary.is_file() {
            // `cargo test -p xpack-cli` does not build other crates' binaries.
            eprintln!("skipping {name}: run `cargo build --workspace` first");
            continue;
        }
        for flag in flags_of(&binary, None) {
            if !doc.contains(&flag) {
                undocumented.push(format!("{name} {flag}"));
            }
        }
    }

    assert!(
        undocumented.is_empty(),
        "these flags exist but appear nowhere in docs/README.md: {undocumented:#?}"
    );
}

#[test]
fn every_environment_variable_is_documented() {
    let Some(doc) = document() else {
        return skipped();
    };

    // Named here rather than scraped from the source: a test that discovered
    // them by grepping would also discover the ones belonging to the test
    // suite, and would quietly stop checking anything if the grep broke.
    for variable in
        ["XPACK_INSTALL_ROOT", "XPACK_APPLICATION_DIR", "XPACK_HEALTH_FILE", "XPACK_LOG"]
    {
        assert!(
            doc.contains(variable),
            "{variable} is read by the code but appears nowhere in docs/README.md"
        );
    }
}

#[test]
fn the_check_can_actually_fail() {
    // A drift check that cannot fail is worse than none: it reports success
    // forever and nobody looks again. This proves the comparison has teeth by
    // running it against a document that is missing a command and a flag.
    let doc = "| `xpack pack` | Build a signed .xpkg |\n--json\n";

    let documented = documented_commands(doc);
    assert!(documented.contains("pack"), "the table parser found nothing at all");
    assert!(
        commands().difference(&documented).count() > 0,
        "a document listing one command should not satisfy the command check"
    );

    let real = flags_of(&xpack(), Some("install"));
    assert!(!real.is_empty(), "install should have flags of its own");
    assert!(
        real.iter().any(|flag| !doc.contains(flag)),
        "a document mentioning one flag should not satisfy the flag check"
    );
}
