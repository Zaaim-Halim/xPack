//! Running a package's hooks at a hook point.
//!
//! Each hook runs in `xpack-hook`, a program of its own, which carries the
//! script engine so that nothing else does. This module decides whether a
//! hook runs at all, starts that program with exactly what it may have, and
//! records what happened, under the installation lock.
//!
//! # Once
//!
//! A hook point runs at most once per version, ever. Before the first of its
//! hooks starts, *started* is appended to the hook record and flushed; a run
//! that never reports back is therefore remembered as interrupted, and is not
//! run again. Nothing here removes a line from the record.
//!
//! # What runs is what was signed
//!
//! The script is read once, hashed, compared with the digest in the signed
//! manifest, and only that copy is run, from a directory made for the run.
//! A script changed on disk after it was verified, or changed while it ran,
//! makes no difference to what runs.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use xpack_core::hooks::{
    Hook, HookEvent, HookPoint, HookRecord, HookRecordLine, Request, Scope, append,
};
use xpack_core::{Error, InstallPaths, InstallScope, Manifest, Result, Sha256Digest, Version};
use xpack_platform::InstallLock;

/// How long `xpack-hook` is given past a hook's own timeout to stop by
/// itself. The runner stops it, and everything it started, at the timeout;
/// this is only the program's own guard, should the runner be gone.
const ENGINE_GRACE_SECONDS: u32 = 5;

/// How many of a hook's last lines of output a failure carries.
const TAIL_LINES: usize = 20;

/// The longest line of a hook's output passed on; the rest of it is cut.
const LINE_LIMIT: usize = 4 * 1024;

/// How much of a hook's output is passed on, to the log and the screen.
/// After it, one line says the rest is not shown, and the rest is read away
/// so the hook never stalls on a full pipe.
const OUTPUT_LIMIT: usize = 1024 * 1024;

/// How `xpack-hook` begins a line that is its own report on how the hook
/// ended. A hook's own lines never begin so; the engine strips it from them.
const ENGINE_PREFIX: &str = "xpack-hook: ";

/// The variables `xpack-hook` is started with from this process's own
/// environment, for every installation: what finding and running programs
/// needs, and nothing that could carry a secret or steer xPack.
const ENGINE_ENVIRONMENT: [&str; 11] = [
    "PATH",
    "SystemRoot",
    "windir",
    "ComSpec",
    "PATHEXT",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "USER",
    "USERNAME",
    "LOGNAME",
];

/// The user's own places, passed only to the hooks of an installation for one
/// user. An installation for everyone is made by an administrator's process,
/// which under `sudo` or UAC can still carry the variables of the user who
/// started it; its hooks must never write into that user's home.
const USER_ENVIRONMENT: [&str; 2] = ["HOME", "USERPROFILE"];

/// Receives each line of a hook's output as it arrives, with the hook point:
/// to print it to a terminal, or show it on an installer's progress page.
pub type LineSink<'a> = &'a (dyn Fn(HookPoint, &str) + Sync);

/// The hooks of one version, and what they are told.
#[derive(Clone, Copy)]
pub struct HookRun<'a> {
    /// The `xpack-hook` program to run them with.
    pub engine: &'a Path,
    /// Who the installation is for.
    pub scope: InstallScope,
    /// The verified manifest of the version whose hooks these are.
    pub manifest: &'a Manifest,
    /// Where that version's payload files are: its directory, or staging
    /// while it is being installed.
    pub scripts: &'a Path,
    /// The version active before the operation, if any.
    pub from_version: Option<&'a Version>,
    /// The version active after it, if any.
    pub to_version: Option<&'a Version>,
    /// Why a version is being undone, for rollback hooks.
    pub cause: Option<&'a str>,
    /// Where each line of a hook's output goes besides the log, as it
    /// arrives: a terminal prints it as `[install.after] …`.
    pub on_line: Option<LineSink<'a>>,
    /// Set by someone else to cancel: the hook running is stopped, with
    /// everything it started, and has failed.
    pub cancel: Option<&'a AtomicBool>,
}

impl HookRun<'_> {
    /// Runs the hooks at `point`, in the order declared, unless the point has
    /// run for this version before.
    ///
    /// The first failure stops the rest, which are recorded *skipped* so they
    /// never run later either, and is returned as [`Error::HookFailed`]. A
    /// hook record that cannot be read runs nothing and is an error.
    pub fn run(&self, lock: &InstallLock, point: HookPoint) -> Result<()> {
        let hooks: Vec<&Hook> = self.manifest.hooks.at(point).collect();
        if hooks.is_empty() {
            return Ok(());
        }
        let paths = lock.paths();
        let version = &self.manifest.application.version;
        let record_file = paths.hook_record_file();
        let record = HookRecord::read(&record_file)?;
        if record.has_run(version, point) {
            // Never run again, whatever happened. Only a point every one of
            // whose hooks succeeded is a success; one that failed, or was
            // interrupted by a crash or a power cut, stays a failure, so the
            // operation it belongs to is undone as for any other failure.
            let scripts = hooks.iter().map(|hook| hook.script.as_str());
            if record.all_succeeded(version, point, scripts) {
                tracing::info!(%version, %point, "hooks ran before; not again");
                return Ok(());
            }
            let first = hooks
                .iter()
                .find(|hook| !record.script_succeeded(version, point, &hook.script))
                .unwrap_or(&hooks[0]);
            return Err(Error::HookFailed {
                point: point.to_string(),
                script: first.script.clone(),
                reason: "it failed, or was interrupted, when it ran before; a hook runs once"
                    .into(),
                output: Vec::new(),
            });
        }
        for (index, hook) in hooks.iter().enumerate() {
            let digest = self.digest_of(hook)?;
            append(&record_file, &self.line(point, hook, digest, HookEvent::Started, None))?;
            let started = Instant::now();
            let outcome = self.run_one(paths, point, hook, digest);
            let seconds = Some(started.elapsed().as_secs());
            match outcome {
                Ok(()) => {
                    append(
                        &record_file,
                        &self.line(point, hook, digest, HookEvent::Succeeded, seconds),
                    )?;
                }
                Err(failure) => {
                    append(
                        &record_file,
                        &self.line(point, hook, digest, HookEvent::Failed, seconds),
                    )?;
                    for skipped in &hooks[index + 1..] {
                        let digest = self.digest_of(skipped)?;
                        append(
                            &record_file,
                            &self.line(point, skipped, digest, HookEvent::Skipped, None),
                        )?;
                    }
                    return Err(Error::HookFailed {
                        point: point.to_string(),
                        script: hook.script.clone(),
                        reason: failure.reason,
                        output: failure.output,
                    });
                }
            }
        }
        Ok(())
    }

    fn digest_of(&self, hook: &Hook) -> Result<Sha256Digest> {
        self.manifest
            .payload
            .files
            .iter()
            .find(|file| file.path == hook.script)
            .map(|file| file.sha256)
            .ok_or_else(|| {
                Error::invalid("hooks", format!("{} is not in the payload", hook.script))
            })
    }

    fn line(
        &self,
        point: HookPoint,
        hook: &Hook,
        sha256: Sha256Digest,
        event: HookEvent,
        seconds: Option<u64>,
    ) -> HookRecordLine {
        HookRecordLine {
            version: self.manifest.application.version.clone(),
            point,
            script: hook.script.clone(),
            sha256,
            event,
            at: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
            seconds,
        }
    }

    fn run_one(
        &self,
        paths: &InstallPaths,
        point: HookPoint,
        hook: &Hook,
        digest: Sha256Digest,
    ) -> std::result::Result<(), Failure> {
        let fail = |reason: String| Failure { reason, output: Vec::new() };
        let runs = paths.hook_runs_dir();
        std::fs::create_dir_all(&runs)
            .map_err(|e| fail(format!("could not make {}: {e}", runs.display())))?;
        let temp = tempfile::Builder::new()
            .prefix("xpack-hook-")
            .tempdir_in(&runs)
            .map_err(|e| fail(format!("could not make its temporary directory: {e}")))?;
        let temp_dir = temp.path().canonicalize().unwrap_or_else(|_| temp.path().to_path_buf());
        let script = self.verified_copy(hook, digest, &temp_dir).map_err(fail)?;
        let data_dir = paths.data_dir();
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| fail(format!("could not make {}: {e}", data_dir.display())))?;

        let version_dir = paths.version_dir(&self.manifest.application.version);
        let request = self.request(paths, point, hook, script, &version_dir, &data_dir, &temp_dir);
        let input = serde_json::to_vec(&request).map_err(|e| fail(e.to_string()))?;

        let mut command = Command::new(self.engine);
        command.env_clear();
        for name in ENGINE_ENVIRONMENT {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        if self.scope == InstallScope::User {
            for name in USER_ENVIRONMENT {
                if let Some(value) = std::env::var_os(name) {
                    command.env(name, value);
                }
            }
        }
        for name in ["TMPDIR", "TMP", "TEMP"] {
            command.env(name, &temp_dir);
        }
        command
            .current_dir(if version_dir.is_dir() { &version_dir } else { &temp_dir })
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        xpack_platform::without_a_console(&mut command);
        xpack_platform::tree::start_as_tree(&mut command);
        let mut child = command
            .spawn()
            .map_err(|e| fail(format!("{} could not be started: {e}", self.engine.display())))?;
        if let Some(mut stdin) = child.stdin.take() {
            // A program that exits before reading its request is reported by
            // its exit code below; its broken pipe says nothing more.
            let _ = stdin.write_all(&input);
        }
        self.watch(&mut child, point, hook.timeout())
    }

    /// The script, read once, checked against its signed digest, and written
    /// into the run's own directory, which is what runs.
    fn verified_copy(
        &self,
        hook: &Hook,
        digest: Sha256Digest,
        temp_dir: &Path,
    ) -> std::result::Result<PathBuf, String> {
        let source =
            hook.script.split('/').fold(self.scripts.to_path_buf(), |path, part| path.join(part));
        let bytes = std::fs::read(&source).map_err(|e| format!("{}: {e}", source.display()))?;
        if xpack_security::hash::sha256(&bytes) != digest {
            return Err(format!(
                "{} is not the file that was signed; it has changed since it was verified",
                hook.script
            ));
        }
        let name = Path::new(&hook.script)
            .file_name()
            .map_or_else(|| "hook.js".into(), std::ffi::OsStr::to_os_string);
        let dir = temp_dir.join(".script");
        let copy = dir.join(name);
        std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&copy, &bytes))
            .map_err(|e| format!("{}: {e}", copy.display()))?;
        Ok(copy)
    }

    #[allow(clippy::too_many_arguments)]
    fn request(
        &self,
        paths: &InstallPaths,
        point: HookPoint,
        hook: &Hook,
        script: PathBuf,
        version_dir: &Path,
        data_dir: &Path,
        temp_dir: &Path,
    ) -> Request {
        let user = self.scope == InstallScope::User;
        Request {
            script,
            point,
            cause: self.cause.map(str::to_string),
            from_version: self.from_version.map(ToString::to_string),
            to_version: self.to_version.map(ToString::to_string),
            scope: if user { Scope::User } else { Scope::Machine },
            application_dir: absolute(paths.root()),
            version_dir: absolute(version_dir),
            data_dir: absolute(data_dir),
            log_dir: absolute(&paths.logs_dir()),
            temp_dir: temp_dir.to_path_buf(),
            home: if user {
                directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
            } else {
                None
            },
            program_data: if user { None } else { program_data() },
            environment: self.manifest.launch.environment.clone(),
            permissions: if user {
                self.manifest.hooks.permissions.user.clone()
            } else {
                self.manifest.hooks.permissions.machine.clone()
            },
            timeout_seconds: hook
                .timeout_seconds
                .unwrap_or(xpack_core::hooks::DEFAULT_TIMEOUT_SECONDS)
                .saturating_add(ENGINE_GRACE_SECONDS),
        }
    }

    /// Waits for `xpack-hook`, passing each line it writes on, until it ends,
    /// its time runs out, or the operation is cancelled.
    fn watch(
        &self,
        child: &mut Child,
        point: HookPoint,
        timeout: Duration,
    ) -> std::result::Result<(), Failure> {
        let deadline = Instant::now() + timeout;
        let lines = read_lines(child.stderr.take());
        let mut tail: VecDeque<String> = VecDeque::new();
        let mut reason: Option<String> = None;
        let mut take = |line: String, tail: &mut VecDeque<String>| {
            // The program's own word on how the hook ended, as opposed to
            // what the hook said.
            if let Some(said) = line.strip_prefix(ENGINE_PREFIX) {
                let said = said.strip_prefix(&format!("{point}: ")).unwrap_or(said);
                tracing::warn!(%point, "{said}");
                reason = Some(said.to_string());
                return;
            }
            tracing::info!(%point, "{line}");
            if let Some(sink) = self.on_line {
                sink(point, &line);
            }
            tail.push_back(line);
            if tail.len() > TAIL_LINES {
                tail.pop_front();
            }
        };
        let stopped = loop {
            while let Ok(line) = lines.try_recv() {
                take(line, &mut tail);
            }
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => {}
                Err(e) => break Err(format!("could not watch it: {e}")),
            }
            if self.cancel.is_some_and(|cancel| cancel.load(Ordering::SeqCst)) {
                let _ = xpack_platform::tree::stop(child);
                break Err("cancelled".to_string());
            }
            if Instant::now() > deadline {
                let _ = xpack_platform::tree::stop(child);
                break Err(format!("still running after {} seconds", timeout.as_secs()));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // What it wrote before it ended, its own report last. The pipe can be
        // held open by something it left behind, so this waits only so long.
        let settle = Instant::now() + Duration::from_secs(2);
        while let Ok(line) = lines.recv_timeout(settle.saturating_duration_since(Instant::now())) {
            take(line, &mut tail);
        }
        let output: Vec<String> = tail.into_iter().collect();
        match stopped {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(Failure {
                reason: reason.unwrap_or_else(|| format!("xpack-hook ended with {status}")),
                output,
            }),
            Err(why) => Err(Failure { reason: why, output }),
        }
    }
}

/// Parses every script `manifest` names, as found in `scripts`, without
/// running any of them: a version whose script cannot run is refused when it
/// arrives, rather than failing when its hook is due.
pub fn check_scripts(engine: &Path, manifest: &Manifest, scripts: &Path) -> Result<()> {
    let mut names: Vec<&str> = manifest.hooks.all().map(|(_, hook)| hook.script.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    if names.is_empty() {
        return Ok(());
    }
    let mut command = Command::new(engine);
    command.env_clear().arg("--check");
    for name in &names {
        command.arg(name.split('/').fold(scripts.to_path_buf(), |path, part| path.join(part)));
    }
    for name in ENGINE_ENVIRONMENT {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    xpack_platform::without_a_console(&mut command);
    let output = command
        .output()
        .map_err(|e| Error::invalid("hooks", format!("{}: {e}", engine.display())))?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let said: String = said.chars().take(LINE_LIMIT).collect();
    Err(Error::invalid(
        "hooks",
        format!(
            "{} {}: a hook script cannot run: {}",
            manifest.application.name,
            manifest.application.version,
            said.trim().replace(&format!("{}", scripts.display()), "")
        ),
    ))
}

/// Why one hook failed, and what it said last.
struct Failure {
    reason: String,
    output: Vec<String>,
}

/// Each line `pipe` carries, on a thread of its own, so a full pipe never
/// stalls the program writing it.
///
/// Read as bytes, so output that is not UTF-8 is shown as best it can be
/// rather than ending the reading. Bounded: each line to [`LINE_LIMIT`], and
/// all of them to [`OUTPUT_LIMIT`], after which only the engine's own report
/// still comes through.
fn read_lines(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut lines = Lines { sender, passed: 0, full: false };
            let mut chunk = [0u8; 8192];
            let mut line = Vec::new();
            let mut cut = false;
            loop {
                let read = match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                for &byte in &chunk[..read] {
                    if byte == b'\n' {
                        lines.emit(&line, cut);
                        line.clear();
                        cut = false;
                    } else if line.len() < LINE_LIMIT {
                        line.push(byte);
                    } else {
                        cut = true;
                    }
                }
            }
            if !line.is_empty() {
                lines.emit(&line, cut);
            }
        });
    }
    receiver
}

/// The lines [`read_lines`] has passed on so far.
struct Lines {
    sender: mpsc::Sender<String>,
    passed: usize,
    full: bool,
}

impl Lines {
    fn emit(&mut self, bytes: &[u8], cut: bool) {
        let text = String::from_utf8_lossy(bytes);
        let mut text = text.trim_end_matches('\r').to_string();
        if cut {
            text.push_str(" …");
        }
        if !text.starts_with(ENGINE_PREFIX) {
            if self.full {
                return;
            }
            if self.passed + text.len() > OUTPUT_LIMIT {
                self.full = true;
                text = "(further output not shown)".into();
            } else {
                self.passed += text.len();
            }
        }
        let _ = self.sender.send(text);
    }
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The machine-wide place for data, which `{programData}` names.
fn program_data() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("ProgramData").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        Some(PathBuf::from("/Library/Application Support"))
    } else {
        Some(PathBuf::from("/var/lib"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of(bytes: &[u8]) -> Vec<String> {
        read_lines(Some(std::io::Cursor::new(bytes.to_vec()))).into_iter().collect()
    }

    #[test]
    fn output_that_is_not_utf_8_is_shown_as_best_it_can_be_and_reading_goes_on() {
        let lines = lines_of(b"before \xff\xfe after\nnext\r\nxpack-hook: install.after: the end");
        assert_eq!(
            lines,
            ["before \u{fffd}\u{fffd} after", "next", "xpack-hook: install.after: the end"]
        );
    }

    #[test]
    fn a_line_too_long_is_cut_and_too_much_output_is_cut_off_but_the_engines_report_still_comes() {
        let mut bytes = vec![b'x'; LINE_LIMIT * 3];
        bytes.push(b'\n');
        for _ in 0..(OUTPUT_LIMIT / 99 + 100) {
            bytes.extend_from_slice(&[b'y'; 99]);
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(b"xpack-hook: install.after: the end\n");
        let lines = lines_of(&bytes);
        assert_eq!(lines[0].len(), LINE_LIMIT + " …".len());
        assert!(lines.iter().map(String::len).sum::<usize>() < OUTPUT_LIMIT + 200);
        assert_eq!(lines[lines.len() - 2], "(further output not shown)");
        assert_eq!(lines[lines.len() - 1], "xpack-hook: install.after: the end");
    }
}
