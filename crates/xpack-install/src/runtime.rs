//! Replacing xPack's programs in an installation, all of them or none.
//!
//! The launcher, the updater, the uninstaller and the others are replaced
//! together when a newer xPack installs over an installation. Each program is
//! three renames, so a replacement of several is many steps, and an
//! installation must never be left with some programs from one release and
//! some from another, or with one missing. So:
//!
//! 1. every new program is written beside its destination and checked;
//! 2. a [`ReplacementJournal`] naming them all is written to disk;
//! 3. each old program is moved aside and the new one moved into place;
//! 4. every destination is checked to hold its new program;
//! 5. the installation records the release it now runs (the caller's part);
//! 6. the old programs and the journal are removed.
//!
//! A failure in steps 1 to 5 puts every old program back. A process that
//! dies part way leaves the journal, and [`resolve_interrupted`] finishes the
//! replacement when every new program is in place, and undoes it otherwise.

use std::path::{Path, PathBuf};

use xpack_core::runtime::{ReplacementEntry, ReplacementJournal};

use xpack_core::{Error, InstallPaths, Result, Sha256Digest, Version};

/// One program to replace: where it lives, and the new one to put there.
#[derive(Debug, Clone)]
pub(crate) struct Program {
    /// Where it lives, inside the installation.
    pub(crate) destination: PathBuf,
    /// The new program, from the installer.
    pub(crate) source: PathBuf,
}

/// A point in the replacement where a test may make it fail or stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// The new program for entry `n` has been written beside its destination.
    Staged(usize),
    /// The journal is on disk.
    Journaled,
    /// Entry `n`'s old program has been moved aside.
    SetAside(usize),
    /// Entry `n`'s new program has been moved into place.
    Swapped(usize),
    /// Every destination holds its new program.
    Verified,
    /// The installation has recorded the new release.
    Recorded,
}

/// What a test asks for at a [`Step`]. Only tests ask for anything but
/// [`Fault::Continue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Fault {
    /// Carry on.
    Continue,
    /// Fail here, as a refused rename would: everything is put back.
    Fail,
    /// Stop here without putting anything back, as a power cut would.
    Crash,
}

/// Replaces `programs` by the new ones, all or none; see the module
/// documentation for the steps. `record` writes the new release into the
/// installation's state, between checking the new programs and removing the
/// old ones.
pub(crate) fn replace(
    paths: &InstallPaths,
    programs: &[Program],
    from: Option<Version>,
    to: Version,
    record: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    replace_with(paths, programs, from, to, record, &mut |_| Fault::Continue)
}

pub(crate) fn replace_with(
    paths: &InstallPaths,
    programs: &[Program],
    from: Option<Version>,
    to: Version,
    record: &mut dyn FnMut() -> Result<()>,
    fault: &mut dyn FnMut(Step) -> Fault,
) -> Result<()> {
    let journal = journal_for(paths, programs, from, to)?;
    let root = paths.root();
    for program in programs {
        remove_old_set_asides(&program.destination);
    }
    match run(paths, &journal, programs, record, fault) {
        Ok(()) => Ok(()),
        Err(Interrupted::Crashed) => Err(Error::invalid(
            "replacing programs",
            "stopped part way, as a test asked; the journal is left for recovery",
        )),
        Err(Interrupted::Failed(error)) => {
            if let Err(undo) = undo(root, &journal) {
                // The journal stays, so the next start or install tries again.
                tracing::error!(%undo, "could not put the old programs back; recovery will retry");
                return Err(error);
            }
            let _ = xpack_core::atomic::remove_file_if_exists(
                &paths.runtime_replacement_journal_file(),
            );
            Err(error)
        }
    }
}

enum Interrupted {
    Failed(Error),
    Crashed,
}

impl From<Error> for Interrupted {
    fn from(error: Error) -> Self {
        Self::Failed(error)
    }
}

fn run(
    paths: &InstallPaths,
    journal: &ReplacementJournal,
    programs: &[Program],
    record: &mut dyn FnMut() -> Result<()>,
    fault: &mut dyn FnMut(Step) -> Fault,
) -> std::result::Result<(), Interrupted> {
    let root = paths.root();
    let mut at = |step: Step| match fault(step) {
        Fault::Continue => Ok(()),
        Fault::Fail => Err(Interrupted::Failed(Error::invalid(
            "replacing programs",
            format!("failed at {step:?}, as a test asked"),
        ))),
        Fault::Crash => Err(Interrupted::Crashed),
    };

    // 1. Stage, and check each staged file is the installer's program.
    for (n, (entry, program)) in journal.entries.iter().zip(programs).enumerate() {
        let staged = journal.staged(root, entry);
        let bytes = std::fs::read(&program.source).map_err(|e| Error::io(&program.source, e))?;
        if bytes.is_empty() {
            return Err(Error::invalid(
                "replacing programs",
                format!("{} is empty and cannot be a program", program.source.display()),
            )
            .into());
        }
        std::fs::write(&staged, &bytes).map_err(|e| Error::io(&staged, e))?;
        crate::installer::set_executable(&staged)?;
        if digest_of(&staged)?.as_bytes() != entry.sha256.as_bytes() {
            return Err(Error::invalid(
                "replacing programs",
                format!("{} does not match the program it was copied from", staged.display()),
            )
            .into());
        }
        at(Step::Staged(n))?;
    }

    // 2. The journal, before any program moves.
    xpack_core::atomic::write_json(&paths.runtime_replacement_journal_file(), journal)?;
    at(Step::Journaled)?;

    // 3. Swap, one program at a time.
    for (n, entry) in journal.entries.iter().enumerate() {
        let destination = journal.destination(root, entry);
        if destination.exists() {
            xpack_platform::rename_when_free(&destination, &journal.set_aside(root, entry))?;
            at(Step::SetAside(n))?;
        }
        xpack_platform::rename_when_free(&journal.staged(root, entry), &destination)?;
        at(Step::Swapped(n))?;
    }

    // 4. Verify.
    for entry in &journal.entries {
        let destination = journal.destination(root, entry);
        if digest_of(&destination)?.as_bytes() != entry.sha256.as_bytes() {
            return Err(Error::invalid(
                "replacing programs",
                format!("{} does not hold its new program", destination.display()),
            )
            .into());
        }
    }
    at(Step::Verified)?;

    // 5. Record, then 6. clean up. After the record nothing is undone: the
    // new programs are in place and checked.
    record()?;
    // Only a stop can happen from here: the record says the new release, so
    // nothing may put the old programs back, and what follows cannot fail.
    if fault(Step::Recorded) == Fault::Crash {
        return Err(Interrupted::Crashed);
    }
    finish(paths, journal);
    Ok(())
}

/// What [`resolve_interrupted`] found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Every new program was in place: the replacement was finished.
    Finished {
        /// The release the installation now runs.
        to: Version,
    },
    /// Not every new program was in place: the old ones were put back.
    Undone,
}

/// Finishes or undoes a replacement a process stopped part way through.
///
/// `record` writes the journal's release into the installation's state when
/// the replacement is finished. Called at the start of every operation, under the
/// installation lock, and by every launcher start through recovery, so a
/// replacement is never left half done for anything to run.
///
/// # Errors
///
/// When there is a journal this build cannot act on (another format, a path
/// outside the installation) or the old programs cannot be put back. The
/// journal is then left as it is, and nothing should run the installation's
/// programs until an installer has resolved it.
pub fn resolve_interrupted(
    paths: &InstallPaths,
    record: &mut dyn FnMut(&ReplacementJournal) -> Result<()>,
) -> Result<Option<Resolution>> {
    let file = paths.runtime_replacement_journal_file();
    if !file.exists() {
        // A replacement that stopped before writing its journal left only
        // staged copies, which nothing else would ever remove.
        remove_stale_staged(paths);
        return Ok(None);
    }
    let journal: ReplacementJournal = xpack_core::atomic::read_json(&file).map_err(|e| {
        Error::invalid(
            "replacing programs",
            format!(
                "an interrupted replacement left a record that cannot be read ({e}); \
                 run the installer again"
            ),
        )
    })?;
    journal.validate()?;
    let root = paths.root();

    let all_new = journal.entries.iter().all(|entry| {
        digest_of(&journal.destination(root, entry))
            .is_ok_and(|digest| digest.as_bytes() == entry.sha256.as_bytes())
    });
    if all_new {
        record(&journal)?;
        finish(paths, &journal);
        tracing::info!(to = %journal.to, "finished an interrupted replacement of the programs");
        return Ok(Some(Resolution::Finished { to: journal.to }));
    }
    // On Windows this can be refused when the program asking is itself one
    // being put back, running from the new file; an installer, run with
    // nothing open, can always do it.
    undo(root, &journal).map_err(|e| {
        Error::invalid(
            "replacing programs",
            format!(
                "an interrupted replacement could not be undone ({e}); run the installer \
                 again"
            ),
        )
    })?;
    xpack_core::atomic::remove_file_if_exists(&file)?;
    tracing::warn!("undid an interrupted replacement of the programs");
    Ok(Some(Resolution::Undone))
}

/// Puts every old program back and removes every staged one.
///
/// Only programs that exist are ever replaced, so every entry has an old
/// program, and it is in one of three places: still at its destination (not
/// yet touched), set aside (moved, the new one perhaps already in its
/// place), or, if both renames are done, set aside with the new one in
/// place. Moving the set-aside file back over the destination covers the
/// last two; the first needs nothing.
fn undo(root: &Path, journal: &ReplacementJournal) -> Result<()> {
    for entry in journal.entries.iter().rev() {
        let aside = journal.set_aside(root, entry);
        if aside.exists() {
            xpack_platform::rename_when_free(&aside, &journal.destination(root, entry))?;
        }
        xpack_core::atomic::remove_file_if_exists(&journal.staged(root, entry))?;
    }
    Ok(())
}

/// Removes what a finished replacement leaves: the old programs and the
/// journal. An old program Windows will not let go of (it is still running,
/// in an installation for everyone) stays until a later install removes it.
fn finish(paths: &InstallPaths, journal: &ReplacementJournal) {
    let root = paths.root();
    for entry in &journal.entries {
        let _ = std::fs::remove_file(journal.set_aside(root, entry));
        let _ = std::fs::remove_file(journal.staged(root, entry));
    }
    let _ = xpack_core::atomic::remove_file_if_exists(&paths.runtime_replacement_journal_file());
}

/// Removes staged copies a replacement left when it stopped before writing
/// its journal: hidden `.<name>.xpack-new…` files in the installation's root
/// and its command directory, the only places programs live.
fn remove_stale_staged(paths: &InstallPaths) {
    let dirs = [paths.root().to_path_buf(), paths.root().join(xpack_core::paths::COMMAND_DIR)];
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(std::result::Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && name.contains(".xpack-new") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Removes old programs earlier replacements set aside beside `destination`
/// and could not remove then, because they were running.
fn remove_old_set_asides(destination: &Path) {
    let (Some(dir), Some(name)) = (destination.parent(), destination.file_name()) else {
        return;
    };
    let prefix = format!("{}.xpack-old-", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(std::result::Result::ok) {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Describes replacing `programs`: each destination relative to the root,
/// and each new program's digest.
fn journal_for(
    paths: &InstallPaths,
    programs: &[Program],
    from: Option<Version>,
    to: Version,
) -> Result<ReplacementJournal> {
    let root = paths.root();
    let mut entries = Vec::with_capacity(programs.len());
    for program in programs {
        let relative = program.destination.strip_prefix(root).map_err(|_| {
            Error::invalid(
                "replacing programs",
                format!("{} is not inside the installation", program.destination.display()),
            )
        })?;
        let destination = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        entries.push(ReplacementEntry { destination, sha256: digest_of(&program.source)? });
    }
    let id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos())
        .to_string();
    let journal = ReplacementJournal::new(id, from, to, entries);
    journal.validate()?;
    Ok(journal)
}

fn digest_of(path: &Path) -> Result<Sha256Digest> {
    let mut file = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    xpack_security::hash::sha256_reader(&mut file).map(|(digest, _)| digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An installation with three programs, two in the root and one in the
    /// command directory, and newer ones beside it to replace them with.
    struct World {
        _dir: tempfile::TempDir,
        paths: InstallPaths,
        programs: Vec<Program>,
    }

    const NAMES: [&str; 3] = ["Example", "Example Updater", "bin/example.exe"];

    impl World {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let paths = InstallPaths::new(&dir.path().join("apps"), "com.example.app").unwrap();
            std::fs::create_dir_all(paths.state_dir()).unwrap();
            std::fs::create_dir_all(paths.root().join("bin")).unwrap();
            let incoming = dir.path().join("incoming");
            std::fs::create_dir_all(&incoming).unwrap();
            let programs = NAMES
                .iter()
                .enumerate()
                .map(|(n, name)| {
                    let destination = paths.root().join(name);
                    std::fs::write(&destination, format!("old {name}")).unwrap();
                    let source = incoming.join(format!("new-{n}"));
                    std::fs::write(&source, format!("new {name}")).unwrap();
                    Program { destination, source }
                })
                .collect();
            Self { _dir: dir, paths, programs }
        }

        fn contents(&self) -> Vec<String> {
            self.programs
                .iter()
                .map(|p| {
                    std::fs::read_to_string(&p.destination).unwrap_or_else(|_| "MISSING".into())
                })
                .collect()
        }

        /// Files a replacement leaves when it does not clean up after itself.
        fn leftovers(&self) -> Vec<String> {
            let mut found = Vec::new();
            for dir in [self.paths.root().to_path_buf(), self.paths.root().join("bin")] {
                for entry in std::fs::read_dir(dir).unwrap().filter_map(std::result::Result::ok) {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.contains(".xpack-new") || name.contains(".xpack-old") {
                        found.push(name);
                    }
                }
            }
            if self.paths.runtime_replacement_journal_file().exists() {
                found.push("the journal".into());
            }
            found
        }

        fn replace(&self, fault: &mut dyn FnMut(Step) -> Fault) -> (Result<()>, u32) {
            let mut recorded = 0;
            let result = replace_with(
                &self.paths,
                &self.programs,
                Some(v("0.6.1")),
                v("0.7.0"),
                &mut || {
                    recorded += 1;
                    Ok(())
                },
                fault,
            );
            (result, recorded)
        }

        fn resolve(&self) -> (Result<Option<Resolution>>, u32) {
            let mut recorded = 0;
            let result = resolve_interrupted(&self.paths, &mut |journal| {
                assert_eq!(journal.to, v("0.7.0"));
                recorded += 1;
                Ok(())
            });
            (result, recorded)
        }
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    /// What every program holds when all are `age` ("old" or "new").
    fn all(age: &str) -> Vec<String> {
        NAMES.iter().map(|name| format!("{age} {name}")).collect()
    }

    /// Every step a replacement of [`NAMES`] passes, in order.
    fn every_step() -> Vec<Step> {
        let n = NAMES.len();
        let mut steps: Vec<Step> = (0..n).map(Step::Staged).collect();
        steps.push(Step::Journaled);
        for i in 0..n {
            steps.push(Step::SetAside(i));
            steps.push(Step::Swapped(i));
        }
        steps.push(Step::Verified);
        steps.push(Step::Recorded);
        steps
    }

    #[test]
    fn every_program_is_replaced_and_nothing_is_left_behind() {
        let world = World::new();
        let (result, recorded) = world.replace(&mut |_| Fault::Continue);

        result.unwrap();
        assert_eq!(world.contents(), all("new"));
        assert_eq!(recorded, 1, "the new release was not recorded exactly once");
        assert!(world.leftovers().is_empty(), "{:?}", world.leftovers());
    }

    #[test]
    fn a_failure_at_any_step_before_the_record_puts_every_old_program_back() {
        for failing in every_step().into_iter().filter(|step| *step != Step::Recorded) {
            let world = World::new();
            let (result, recorded) = world.replace(&mut |step| {
                if step == failing { Fault::Fail } else { Fault::Continue }
            });

            assert!(result.is_err(), "{failing:?}: reported success");
            assert_eq!(world.contents(), all("old"), "{failing:?}: a mixture was left");
            assert_eq!(recorded, 0, "{failing:?}: the new release was recorded");
            assert!(world.leftovers().is_empty(), "{failing:?}: {:?}", world.leftovers());
        }
    }

    #[test]
    fn a_record_that_cannot_be_written_puts_every_old_program_back() {
        let world = World::new();
        let result = replace_with(
            &world.paths,
            &world.programs,
            None,
            v("0.7.0"),
            &mut || Err(Error::invalid("state", "the disk is full")),
            &mut |_| Fault::Continue,
        );

        assert!(result.is_err());
        assert_eq!(world.contents(), all("old"));
        assert!(world.leftovers().is_empty(), "{:?}", world.leftovers());
    }

    #[test]
    fn a_stop_at_any_step_is_finished_or_undone_by_recovery_never_left_mixed() {
        // What a power cut does: nothing is put back by the process that
        // stopped. Whoever opens the installation next must make it whole.
        for stopping in every_step() {
            let world = World::new();
            let (result, during) = world.replace(&mut |step| {
                if step == stopping { Fault::Crash } else { Fault::Continue }
            });
            assert!(result.is_err(), "{stopping:?}");

            let (resolved, recorded) = world.resolve();
            let resolved = resolved.unwrap();
            let contents = world.contents();

            // Once every new program is in place, it is finished; before, undone.
            let all_swapped = matches!(stopping, Step::Swapped(i) if i == NAMES.len() - 1)
                || matches!(stopping, Step::Verified | Step::Recorded);
            if all_swapped {
                assert_eq!(contents, all("new"), "{stopping:?}: not finished");
                assert_eq!(resolved, Some(Resolution::Finished { to: v("0.7.0") }), "{stopping:?}");
                assert_eq!(recorded, 1, "{stopping:?}: the finished release was not recorded");
            } else {
                assert_eq!(contents, all("old"), "{stopping:?}: not undone");
                assert_eq!(recorded + during, 0, "{stopping:?}: an undone release was recorded");
                let journaled = !matches!(stopping, Step::Staged(_));
                assert_eq!(resolved.is_some(), journaled, "{stopping:?}: {resolved:?}");
            }
            assert!(world.leftovers().is_empty(), "{stopping:?}: {:?}", world.leftovers());

            // And it stays resolved: a second look finds nothing to do.
            assert_eq!(world.resolve().0.unwrap(), None, "{stopping:?}");
        }
    }

    #[test]
    fn a_journal_naming_a_file_outside_the_installation_is_refused_and_nothing_moves() {
        // Recovery renames what a journal names; one edited on disk must not
        // make it touch anything else.
        let world = World::new();
        let (result, _) = world.replace(&mut |step| {
            if step == Step::SetAside(1) { Fault::Crash } else { Fault::Continue }
        });
        assert!(result.is_err());
        let file = world.paths.runtime_replacement_journal_file();
        let mut journal: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        journal["entries"][0]["destination"] = serde_json::json!("../../outside");
        std::fs::write(&file, serde_json::to_vec(&journal).unwrap()).unwrap();
        let before = world.contents();

        let error = world.resolve().0.unwrap_err().to_string();

        assert!(error.contains("outside") || error.contains(".."), "{error}");
        assert_eq!(world.contents(), before, "a refused journal still moved files");
        assert!(file.exists(), "a refused journal was removed, hiding the problem");
    }

    #[test]
    fn a_journal_that_cannot_be_read_is_refused_with_what_to_do() {
        let world = World::new();
        std::fs::write(world.paths.runtime_replacement_journal_file(), b"{ torn").unwrap();

        let error = world.resolve().0.unwrap_err().to_string();

        assert!(error.contains("run the installer again"), "{error}");
    }

    #[test]
    fn programs_set_aside_by_an_earlier_replacement_do_not_stand_in_the_way() {
        // Windows keeps a program set aside while it still runs; the next
        // replacement removes it when it can and is never blocked by it.
        let world = World::new();
        let stale = world.paths.root().join("Example.xpack-old-1");
        std::fs::write(&stale, "left by an earlier replacement").unwrap();

        world.replace(&mut |_| Fault::Continue).0.unwrap();

        assert_eq!(world.contents(), all("new"));
        assert!(!stale.exists());
    }
}
