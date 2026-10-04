//! Scripts a package runs at chosen moments of an installation's life.
//!
//! A package can ship JavaScript files, in its payload like every other file,
//! and name in its manifest the moments they run at: when the application is
//! installed, when an update to a new version is applied, when one is undone,
//! and when it is uninstalled. Each such moment is a [`HookPoint`]: an
//! [`Operation`] and a [`Moment`] within it.
//!
//! This module holds what a manifest says about hooks and the rules it must
//! keep, and the hook record: the installation's account of which hook points
//! have run, which is what makes each run at most once. Running a hook is not
//! done here.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::digest::Sha256Digest;
use crate::error::{Error, Result};
use crate::manifest::{PayloadSpec, validate_relative_path};
use crate::version::Version;

/// The largest hook script a package may carry.
pub const MAX_SCRIPT_BYTES: u64 = 1024 * 1024;

/// How long a hook may run when its manifest entry does not say.
pub const DEFAULT_TIMEOUT_SECONDS: u32 = 300;

/// The longest a hook may be allowed to run.
pub const MAX_TIMEOUT_SECONDS: u32 = 3600;

/// What happens to an installation while a hook runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Operation {
    /// The application is installed for the first time.
    Install,
    /// A new version is applied.
    Update,
    /// A version is undone and the one before it made active again.
    Rollback,
    /// The application is removed.
    Uninstall,
}

impl Operation {
    /// Every operation, in the order an installation meets them.
    pub const ALL: [Self; 4] = [Self::Install, Self::Update, Self::Rollback, Self::Uninstall];

    /// The moments a hook can run at during this operation.
    pub fn moments(self) -> &'static [Moment] {
        match self {
            Self::Install => &[Moment::Before, Moment::AfterFiles, Moment::After],
            Self::Update => &[Moment::Before, Moment::After, Moment::Confirmed],
            Self::Rollback | Self::Uninstall => &[Moment::Before, Moment::After],
        }
    }

    /// The moment a hook that does not say runs at.
    ///
    /// After the step for installing, updating and rolling back, when what
    /// the hook works on is in place; before it for uninstalling, while it
    /// still is.
    pub fn default_moment(self) -> Moment {
        match self {
            Self::Uninstall => Moment::Before,
            Self::Install | Self::Update | Self::Rollback => Moment::After,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Update => "update",
            Self::Rollback => "rollback",
            Self::Uninstall => "uninstall",
        }
    }
}

/// When within an operation a hook runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Moment {
    /// Before the operation changes anything.
    Before,
    /// After a first installation's files are in place, before it is made
    /// active. Installing only.
    AfterFiles,
    /// After the operation's change.
    After,
    /// After a new version has proved it starts. Updating only.
    Confirmed,
}

impl Moment {
    fn name(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::AfterFiles => "afterFiles",
            Self::After => "after",
            Self::Confirmed => "confirmed",
        }
    }
}

/// An operation and a moment in it: where a hook runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HookPoint {
    /// The operation.
    pub operation: Operation,
    /// The moment within it.
    pub moment: Moment,
}

impl fmt::Display for HookPoint {
    /// `install.afterFiles`, `update.confirmed`: what logs and records say.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.operation.name(), self.moment.name())
    }
}

impl FromStr for HookPoint {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        let unknown = || Error::invalid("hook point", format!("{text:?} is not one"));
        let (operation, moment) = text.split_once('.').ok_or_else(unknown)?;
        let operation =
            Operation::ALL.into_iter().find(|o| o.name() == operation).ok_or_else(unknown)?;
        let moment =
            operation.moments().iter().copied().find(|m| m.name() == moment).ok_or_else(unknown)?;
        Ok(Self { operation, moment })
    }
}

impl Serialize for HookPoint {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for HookPoint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// One script, and when it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hook {
    /// The script: a `.js` file of the payload, relative to it.
    pub script: String,
    /// The moment it runs at; the operation's [default](Operation::default_moment)
    /// when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Moment>,
    /// How long it may run, in seconds; [`DEFAULT_TIMEOUT_SECONDS`] when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

impl Hook {
    /// The moment it runs at, during `operation`.
    pub fn moment(&self, operation: Operation) -> Moment {
        self.when.unwrap_or_else(|| operation.default_moment())
    }

    /// How long it may run.
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(u64::from(self.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS)))
    }
}

/// What a package may let its hooks do, for one kind of installation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScopePermissions {
    /// Programs a hook may run, by file name or absolute path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exec: Vec<String>,
    /// Places beyond the installation's own where a hook may write, each
    /// starting with a placeholder (`{home}`, `{tempDir}`, `{programData}`)
    /// or, for an installation for everyone, an absolute path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write: Vec<String>,
}

impl ScopePermissions {
    /// Whether nothing is declared.
    pub fn is_empty(&self) -> bool {
        self.exec.is_empty() && self.write.is_empty()
    }
}

/// What hooks may do, for each kind of installation. Only the block for the
/// installation's own scope is in force.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Permissions {
    /// In an installation for one user.
    #[serde(default, skip_serializing_if = "ScopePermissions::is_empty")]
    pub user: ScopePermissions,
    /// In an installation for everyone on the machine.
    #[serde(default, skip_serializing_if = "ScopePermissions::is_empty")]
    pub machine: ScopePermissions,
}

impl Permissions {
    /// Whether nothing is declared for either scope.
    pub fn is_empty(&self) -> bool {
        self.user.is_empty() && self.machine.is_empty()
    }
}

/// A manifest's `hooks`: the scripts it runs, by operation, and what they
/// may do.
///
/// Each operation takes one hook or a list. A hook is a script path, short
/// for `{ "script": path }`, or an object with `when` and `timeoutSeconds`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hooks {
    /// What the hooks may do.
    #[serde(default, skip_serializing_if = "Permissions::is_empty")]
    pub permissions: Permissions,
    /// Hooks of a first installation.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "one_or_many")]
    pub install: Vec<Hook>,
    /// Hooks of applying a new version.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "one_or_many")]
    pub update: Vec<Hook>,
    /// Hooks of undoing a version.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "one_or_many")]
    pub rollback: Vec<Hook>,
    /// Hooks of uninstalling.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "one_or_many")]
    pub uninstall: Vec<Hook>,
}

impl Hooks {
    /// Whether the package declares nothing about hooks at all.
    pub fn is_empty(&self) -> bool {
        self.permissions.is_empty() && Operation::ALL.iter().all(|op| self.of(*op).is_empty())
    }

    /// The hooks of one operation, in the order declared.
    pub fn of(&self, operation: Operation) -> &[Hook] {
        match operation {
            Operation::Install => &self.install,
            Operation::Update => &self.update,
            Operation::Rollback => &self.rollback,
            Operation::Uninstall => &self.uninstall,
        }
    }

    /// The hooks that run at `point`, in the order declared.
    pub fn at(&self, point: HookPoint) -> impl Iterator<Item = &Hook> {
        self.of(point.operation)
            .iter()
            .filter(move |hook| hook.moment(point.operation) == point.moment)
    }

    /// Every hook, with the point it runs at.
    pub fn all(&self) -> impl Iterator<Item = (HookPoint, &Hook)> {
        Operation::ALL.into_iter().flat_map(move |operation| {
            self.of(operation)
                .iter()
                .map(move |hook| (HookPoint { operation, moment: hook.moment(operation) }, hook))
        })
    }

    /// Enforces every rule a manifest's hooks must keep, against the payload
    /// they come from.
    ///
    /// # Errors
    ///
    /// Naming the hook and the rule, when a `when` is not one its operation
    /// has, a timeout is outside 1–3600 seconds, a script is listed twice at
    /// one point, a script is not a `.js` file of the payload or is larger
    /// than [`MAX_SCRIPT_BYTES`], permissions are declared with no hook, or a
    /// permission cannot be allowed in its scope.
    pub fn validate(&self, payload: &PayloadSpec) -> Result<()> {
        let mut seen: BTreeSet<(HookPoint, String)> = BTreeSet::new();
        let mut any = false;
        for (point, hook) in self.all() {
            any = true;
            let subject = format!("hooks.{}", point.operation.name());
            if let Some(when) = hook.when
                && !point.operation.moments().contains(&when)
            {
                return Err(Error::invalid(
                    &subject,
                    format!(
                        "{:?} is not a moment of {}; it has {}",
                        when.name(),
                        point.operation.name(),
                        point
                            .operation
                            .moments()
                            .iter()
                            .map(|m| m.name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
            if let Some(timeout) = hook.timeout_seconds
                && !(1..=MAX_TIMEOUT_SECONDS).contains(&timeout)
            {
                return Err(Error::invalid(
                    &subject,
                    format!("timeoutSeconds is {timeout}; it must be 1 to {MAX_TIMEOUT_SECONDS}"),
                ));
            }
            validate_script(&subject, &hook.script, payload)?;
            if !seen.insert((point, hook.script.replace('\\', "/"))) {
                return Err(Error::invalid(
                    &subject,
                    format!("{} is listed twice at {point}", hook.script),
                ));
            }
        }
        if !any && !self.permissions.is_empty() {
            return Err(Error::invalid(
                "hooks.permissions",
                "declares what hooks may do, but there are no hooks",
            ));
        }
        validate_scope("hooks.permissions.user", &self.permissions.user, Scope::User)?;
        validate_scope("hooks.permissions.machine", &self.permissions.machine, Scope::Machine)
    }
}

/// A script: a safe relative path, a `.js` file, present in the payload and
/// no larger than [`MAX_SCRIPT_BYTES`].
fn validate_script(subject: &str, script: &str, payload: &PayloadSpec) -> Result<()> {
    validate_relative_path(subject, script)?;
    let path = script.replace('\\', "/");
    if !Path::new(&path).extension().is_some_and(|ext| ext.eq_ignore_ascii_case("js")) {
        return Err(Error::invalid(subject, format!("{script} is not a .js file")));
    }
    let Some(file) = payload.files.iter().find(|file| file.path == path) else {
        return Err(Error::invalid(subject, format!("{script} is not a file of the payload")));
    };
    if file.size > MAX_SCRIPT_BYTES {
        return Err(Error::invalid(
            subject,
            format!("{script} is {} bytes; a hook script may be {MAX_SCRIPT_BYTES}", file.size),
        ));
    }
    Ok(())
}

/// Who an installation is for, and so whose rights its hooks run with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Scope {
    /// One user, with that user's rights.
    User,
    /// Everyone on the machine, with an administrator's rights.
    Machine,
}

/// The hook interface this release's `xpack-hook` serves: the `ctx` a
/// script is given. Package format 6 is interface 1; a new `ctx` member, or a
/// changed meaning, is a new package format and a new interface.
pub const HOOK_INTERFACE: u32 = 1;

/// Programs that would give a hook more rights than the installation's own,
/// refused for an installation for one user, whatever it declares.
pub const ELEVATION_PROGRAMS: [&str; 6] = ["sudo", "doas", "pkexec", "su", "runas", "osascript"];

/// Whether `program`, a file name or a path, is one of the
/// [`ELEVATION_PROGRAMS`] however it is spelled.
///
/// Windows matches names without regard to case, runs a name given without
/// its extension, and drops trailing dots and spaces, so `RUNAS.EXE`,
/// `runas.com` and `runas.exe.` all start `runas`.
pub fn asks_for_administrator(program: &str) -> bool {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let name = name.trim_end_matches(['.', ' ']).to_ascii_lowercase();
    let stem = [".exe", ".com", ".bat", ".cmd"]
        .iter()
        .find_map(|extension| name.strip_suffix(extension))
        .unwrap_or(&name)
        .trim_end_matches(['.', ' ']);
    ELEVATION_PROGRAMS.contains(&stem)
}

fn validate_scope(subject: &str, permissions: &ScopePermissions, scope: Scope) -> Result<()> {
    for program in &permissions.exec {
        let refuse = |why: &str| Err(Error::invalid(subject, format!("exec {program:?} {why}")));
        if program.trim().is_empty() || program.contains('\0') {
            return refuse("is empty or holds a NUL");
        }
        let absolute = program.starts_with('/') || is_drive_absolute(program);
        if !absolute && (program.contains('/') || program.contains('\\')) {
            return refuse("must be a program's file name or an absolute path");
        }
        if scope == Scope::User && asks_for_administrator(program) {
            return refuse("asks for administrator rights, which a hook for one user never gets");
        }
    }
    for place in &permissions.write {
        validate_place(subject, place, scope)?;
    }
    Ok(())
}

/// A place a hook may write: a placeholder its scope allows, then a plain
/// relative path; or, for everyone, an absolute path.
fn validate_place(subject: &str, place: &str, scope: Scope) -> Result<()> {
    let refuse = |why: String| Err(Error::invalid(subject, format!("write {place:?} {why}")));
    let allowed: &[&str] = match scope {
        Scope::User => &["{home}", "{tempDir}"],
        Scope::Machine => &["{programData}", "{tempDir}"],
    };
    let rest = if let Some(placeholder) = allowed.iter().find(|p| place.starts_with(**p)) {
        &place[placeholder.len()..]
    } else if place.starts_with('{') {
        return refuse(format!(
            "names a place an installation like this cannot write; it may use {}",
            allowed.join(", ")
        ));
    } else if scope == Scope::Machine && (place.starts_with('/') || is_drive_absolute(place)) {
        place
    } else if scope == Scope::User {
        return refuse(
            "is outside the user's own places; an installation for one user may write only \
             under {home} or {tempDir}"
                .to_string(),
        );
    } else {
        return refuse(
            "must start with {programData}, {tempDir} or be an absolute path".to_string(),
        );
    };
    let normalised = rest.replace('\\', "/");
    if normalised.contains('\0') || normalised.split('/').any(|part| part == ".." || part == ".") {
        return refuse("has a '.' or '..' component".to_string());
    }
    if !rest.is_empty() && !normalised.starts_with('/') && place != rest {
        return refuse("must continue with a '/' after its placeholder".to_string());
    }
    Ok(())
}

fn is_drive_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

/// Accepts one hook or a list of them, each a path or an object.
fn one_or_many<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<Hook>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum One {
        Path(String),
        Full(Hook),
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        Many(Vec<One>),
        One(One),
    }
    let into_hook = |one: One| match one {
        One::Path(script) => Hook { script, when: None, timeout_seconds: None },
        One::Full(hook) => hook,
    };
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::Many(many) => many.into_iter().map(into_hook).collect(),
        OneOrMany::One(one) => vec![into_hook(one)],
    })
}

// --- the hook record ---------------------------------------------------------

// --- what the program that runs a hook is told ---

/// One hook to run, and everything it is given.
///
/// Sent to `xpack-hook` by the installer, launcher or uninstaller as one JSON document on
/// standard input, read whole before the script starts: the script itself
/// gets no input.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    /// The script, an absolute path inside [`Self::version_dir`] or
    /// [`Self::temp_dir`].
    pub script: PathBuf,
    /// Where it runs.
    pub point: HookPoint,
    /// Why a version is being undone: `failedToStart`, `hookFailed` or
    /// `requested`. Rollback only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    /// The version active before; absent for a first installation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_version: Option<String>,
    /// The version active after; absent for uninstalling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_version: Option<String>,
    /// Who the installation is for.
    pub scope: Scope,
    /// The installation's root.
    pub application_dir: PathBuf,
    /// The directory of the version that ships the hook.
    pub version_dir: PathBuf,
    /// The installation's data directory, for hooks and the application.
    pub data_dir: PathBuf,
    /// Where the installation's logs go.
    pub log_dir: PathBuf,
    /// A directory made for this run and removed after it.
    pub temp_dir: PathBuf,
    /// The user's home; an installation for one user only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<PathBuf>,
    /// The machine-wide data place; an installation for everyone only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_data: Option<PathBuf>,
    /// The application's launch environment.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    /// The permissions in force: the package's block for this scope.
    #[serde(default)]
    pub permissions: ScopePermissions,
    /// How long the hook may run.
    pub timeout_seconds: u32,
    /// Set by `xpack hooks test`: record what the hook does, and perhaps do
    /// none of it. Never sent otherwise, so an `xpack-hook` older than this
    /// field still reads every ordinary request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Plan>,
}

/// How a hook run under test is recorded, for one hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Plan {
    /// Where each thing the hook does, or tries to, is appended as a line of
    /// JSON, a [`PlanLine`].
    pub record: PathBuf,
    /// Whether to do it too. Without, programs are not run and nothing is
    /// written, moved or removed: what would be is recorded.
    pub perform: bool,
    /// What a program answers when it is not run, by the name the hook runs
    /// it by. One without an answer succeeds with no output.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub answers: BTreeMap<String, ProgramAnswer>,
    /// The version whose hook this is, for the record.
    pub version: Version,
    /// The script, as the manifest names it, for the record.
    pub script: String,
}

/// What a program not run under test answers, as `ctx.exec` returns it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProgramAnswer {
    /// Its exit code.
    #[serde(default)]
    pub exit_code: i32,
    /// What it wrote to standard output.
    #[serde(default)]
    pub stdout: String,
    /// What it wrote to standard error.
    #[serde(default)]
    pub stderr: String,
}

/// One thing a hook did, or would have done, or was refused, under test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanLine {
    /// The version whose hook it is.
    pub version: Version,
    /// Where it ran.
    pub point: HookPoint,
    /// The script, as the manifest names it.
    pub script: String,
    /// What it was.
    pub action: PlanAction,
    /// Whether it was done, rather than only recorded.
    pub performed: bool,
}

/// What a hook did, or tried to do, through `ctx`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum PlanAction {
    /// Ran a program.
    Exec {
        /// The program, as the hook named it.
        program: String,
        /// Its arguments.
        args: Vec<String>,
    },
    /// Wrote a file, whole.
    Write {
        /// Where.
        path: PathBuf,
    },
    /// Copied a file.
    Copy {
        /// From where.
        from: PathBuf,
        /// To where.
        to: PathBuf,
    },
    /// Moved or renamed a file or directory.
    Move {
        /// From where.
        from: PathBuf,
        /// To where.
        to: PathBuf,
    },
    /// Removed a file or directory.
    Remove {
        /// What.
        path: PathBuf,
    },
    /// Made a directory.
    MakeDir {
        /// Where.
        path: PathBuf,
    },
    /// Was refused something its permissions do not allow.
    Refused {
        /// What it asked for, and why it was refused.
        reason: String,
    },
    /// The hook ended: written by what ran it, so the test knows how each
    /// hook ended even once the installation that ran it is gone.
    Ended {
        /// Whether it succeeded.
        succeeded: bool,
        /// Why it failed, when it did.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl Request {
    /// Checks the request makes sense before anything runs.
    pub fn check(&self) -> Result<(), String> {
        for (name, path) in [
            ("script", &self.script),
            ("applicationDir", &self.application_dir),
            ("versionDir", &self.version_dir),
            ("dataDir", &self.data_dir),
            ("logDir", &self.log_dir),
            ("tempDir", &self.temp_dir),
        ] {
            if !path.is_absolute() {
                return Err(format!("{name} {} is not an absolute path", path.display()));
            }
        }
        // In its version, or a copy in the run's temporary directory: the
        // runner runs a copy it has checked against the signed hash, and
        // before installing there is no version directory to run it from.
        if !self.script.starts_with(&self.version_dir) && !self.script.starts_with(&self.temp_dir) {
            return Err(format!(
                "the script {} is in neither its version's directory nor this run's",
                self.script.display()
            ));
        }
        match self.scope {
            Scope::User if self.program_data.is_some() => {
                Err("an installation for one user has no programData".into())
            }
            Scope::Machine if self.home.is_some() => {
                Err("an installation for everyone has no user's home".into())
            }
            _ if self.timeout_seconds == 0 => Err("timeoutSeconds is 0".into()),
            _ => Ok(()),
        }
    }
}

/// What happened to a hook, as one line of the hook record says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HookEvent {
    /// About to run: written, and flushed to disk, before it starts.
    Started,
    /// Returned, or its promise resolved.
    Succeeded,
    /// Threw, rejected, timed out, was cancelled or could not start.
    Failed,
    /// Not run, because a hook before it at the same point failed.
    Skipped,
}

/// One line of the hook record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRecordLine {
    /// The version whose hook it is.
    pub version: Version,
    /// Where it ran.
    pub point: HookPoint,
    /// The script, as the manifest names it.
    pub script: String,
    /// The script's digest, so the record says exactly what ran.
    pub sha256: Sha256Digest,
    /// What happened.
    pub event: HookEvent,
    /// When, in seconds since the Unix epoch.
    pub at: u64,
    /// How long it ran, for the line that ends a run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
}

/// How a hook point ended for a version, by the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointOutcome {
    /// Started and never ended: interrupted. Counts as failed.
    Interrupted,
    /// Every hook at the point succeeded.
    Succeeded,
    /// A hook failed, or the run was interrupted part way.
    Failed,
}

/// The installation's hook record, read: which hook points have run, for
/// which version, and how they ended.
///
/// A hook point in the record has run, whatever its outcome, and never runs
/// again for that version: nothing removes a line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookRecord {
    points: BTreeMap<(Version, HookPoint), PointOutcome>,
    /// Each script recorded as having succeeded, at its version and point.
    succeeded: BTreeSet<(Version, HookPoint, String)>,
}

impl HookRecord {
    /// Reads the record at `path`. No file is an empty record.
    ///
    /// A line that cannot be read, as a write torn by a power cut leaves,
    /// still counts as a start of the version and point it names when those
    /// can be made out: the hook may have run, so it must not run again.
    ///
    /// # Errors
    ///
    /// When the file cannot be read, or holds a line naming no version and
    /// point that can be made out. Then nothing can say which hooks have run,
    /// and none should run until the record is repaired.
    pub fn read(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(Error::io(path, error)),
        };
        let mut record = Self::default();
        for (number, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<HookRecordLine>(line) {
                Ok(line) => record.note(&line),
                Err(_) => match salvage(line) {
                    Some(key) => {
                        record.points.entry(key).or_insert(PointOutcome::Interrupted);
                    }
                    None => {
                        return Err(Error::invalid(
                            "hook record",
                            format!(
                                "line {} of {} cannot be read, so which hooks have run is not \
                                 known; no hook runs until it is repaired",
                                number + 1,
                                path.display()
                            ),
                        ));
                    }
                },
            }
        }
        Ok(record)
    }

    fn note(&mut self, line: &HookRecordLine) {
        if line.event == HookEvent::Succeeded {
            self.succeeded.insert((line.version.clone(), line.point, line.script.clone()));
        }
        let entry = self
            .points
            .entry((line.version.clone(), line.point))
            .or_insert(PointOutcome::Interrupted);
        *entry = match (line.event, *entry) {
            // A failure anywhere at the point is the point's outcome.
            (HookEvent::Failed | HookEvent::Skipped, _) | (_, PointOutcome::Failed) => {
                PointOutcome::Failed
            }
            (HookEvent::Started, _) => PointOutcome::Interrupted,
            (HookEvent::Succeeded, _) => PointOutcome::Succeeded,
        };
    }

    /// Whether `point` has run for `version`, in any way.
    pub fn has_run(&self, version: &Version, point: HookPoint) -> bool {
        self.points.contains_key(&(version.clone(), point))
    }

    /// How `point` ended for `version`, if it has run.
    pub fn outcome(&self, version: &Version, point: HookPoint) -> Option<PointOutcome> {
        self.points.get(&(version.clone(), point)).copied()
    }

    /// Whether every one of `scripts` ran to success at `point` for
    /// `version`, and nothing there failed.
    ///
    /// A point can read as succeeded with a script of it never started: the
    /// machine stopped between one hook's success and the next one's start.
    /// So a point that ran is only a success when each script declared at it
    /// says so.
    pub fn all_succeeded<'a>(
        &self,
        version: &Version,
        point: HookPoint,
        scripts: impl IntoIterator<Item = &'a str>,
    ) -> bool {
        self.outcome(version, point) == Some(PointOutcome::Succeeded)
            && scripts.into_iter().all(|script| self.script_succeeded(version, point, script))
    }

    /// Whether `script` is recorded as having succeeded at `point` for
    /// `version`.
    pub fn script_succeeded(&self, version: &Version, point: HookPoint, script: &str) -> bool {
        self.succeeded.contains(&(version.clone(), point, script.to_string()))
    }

    /// Whether any of `version`'s hooks at points of `operation` has started.
    pub fn any_started(&self, version: &Version, operation: Operation) -> bool {
        self.points.keys().any(|(v, point)| v == version && point.operation == operation)
    }
}

/// The version and point a torn line names, if both can be made out.
fn salvage(line: &str) -> Option<(Version, HookPoint)> {
    let field = |name: &str| -> Option<String> {
        let start = line.find(&format!("\"{name}\":\""))? + name.len() + 4;
        let end = line[start..].find('"')? + start;
        Some(line[start..end].to_string())
    };
    let version = Version::parse(&field("version")?).ok()?;
    let point = field("point")?.parse().ok()?;
    Some((version, point))
}

/// Appends `line` to the record at `path` and flushes it to disk before
/// returning, so a hook started after this is recorded as started even if
/// the machine loses power while it runs.
///
/// # Errors
///
/// When the record cannot be opened, written or flushed.
pub fn append(path: &Path, line: &HookRecordLine) -> Result<()> {
    let created = !path.exists();
    if let Some(parent) = path.parent() {
        crate::atomic::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string(line).map_err(|e| Error::json("hook record", e))?;
    text.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| Error::io(path, e))?;
    file.write_all(text.as_bytes()).map_err(|e| Error::io(path, e))?;
    file.sync_all().map_err(|e| Error::io(path, e))?;
    if created && let Some(parent) = path.parent() {
        crate::atomic::sync_dir(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::PayloadFile;

    fn payload(files: &[(&str, u64)]) -> PayloadSpec {
        PayloadSpec {
            total_size: files.iter().map(|(_, size)| size).sum(),
            files: files
                .iter()
                .map(|(path, size)| PayloadFile {
                    path: (*path).to_string(),
                    size: *size,
                    sha256: Sha256Digest::from_bytes([0; crate::SHA256_LEN]),
                    mode: None,
                })
                .collect(),
        }
    }

    fn scripts() -> PayloadSpec {
        payload(&[("xpack/hooks/a.js", 100), ("xpack/hooks/b.js", 100), ("bin/app", 10)])
    }

    fn hooks(json: &str) -> Hooks {
        serde_json::from_str(json).unwrap()
    }

    fn refused(json: &str) -> String {
        hooks(json).validate(&scripts()).unwrap_err().to_string()
    }

    fn point(text: &str) -> HookPoint {
        text.parse().unwrap()
    }

    #[test]
    fn a_hook_is_a_path_or_an_object_and_an_operation_takes_one_or_a_list() {
        let parsed = hooks(
            r#"{
                "install": "xpack/hooks/a.js",
                "update": [
                    { "when": "before", "script": "xpack/hooks/a.js" },
                    { "script": "xpack/hooks/b.js", "timeoutSeconds": 600 }
                ],
                "uninstall": { "script": "xpack/hooks/b.js", "when": "after" }
            }"#,
        );
        parsed.validate(&scripts()).unwrap();
        assert_eq!(
            parsed.install,
            [Hook { script: "xpack/hooks/a.js".into(), when: None, timeout_seconds: None }]
        );
        assert_eq!(parsed.update.len(), 2);
        assert_eq!(parsed.update[1].timeout(), Duration::from_secs(600));
        assert_eq!(parsed.install[0].timeout(), Duration::from_secs(300));
        assert_eq!(parsed.uninstall[0].moment(Operation::Uninstall), Moment::After);
    }

    #[test]
    fn a_hook_that_does_not_say_runs_at_its_operations_default_moment() {
        let parsed = hooks(
            r#"{"install":"xpack/hooks/a.js","update":"xpack/hooks/a.js",
                "rollback":"xpack/hooks/a.js","uninstall":"xpack/hooks/a.js"}"#,
        );
        let points: Vec<String> = parsed.all().map(|(p, _)| p.to_string()).collect();
        assert_eq!(points, ["install.after", "update.after", "rollback.after", "uninstall.before"]);
    }

    #[test]
    fn the_hooks_at_a_point_come_in_the_order_declared() {
        let parsed = hooks(
            r#"{"update":[{"when":"before","script":"xpack/hooks/b.js"},
                          {"when":"after","script":"xpack/hooks/a.js"},
                          {"when":"before","script":"xpack/hooks/a.js"}]}"#,
        );
        let before: Vec<&str> =
            parsed.at(point("update.before")).map(|h| h.script.as_str()).collect();
        assert_eq!(before, ["xpack/hooks/b.js", "xpack/hooks/a.js"]);
    }

    #[test]
    fn every_hook_point_reads_back_as_written_and_no_other_does() {
        for operation in Operation::ALL {
            for moment in operation.moments() {
                let p = HookPoint { operation, moment: *moment };
                assert_eq!(p.to_string().parse::<HookPoint>().unwrap(), p);
            }
        }
        for wrong in [
            "install.confirmed",
            "update.afterFiles",
            "uninstall.confirmed",
            "install",
            "start.before",
            "",
        ] {
            assert!(wrong.parse::<HookPoint>().is_err(), "{wrong:?} was accepted");
        }
    }

    #[test]
    fn a_moment_its_operation_does_not_have_is_refused() {
        assert!(
            refused(r#"{"update":{"when":"afterFiles","script":"xpack/hooks/a.js"}}"#)
                .contains("not a moment of update")
        );
        assert!(
            refused(r#"{"install":{"when":"confirmed","script":"xpack/hooks/a.js"}}"#)
                .contains("not a moment of install")
        );
        assert!(
            refused(r#"{"uninstall":{"when":"confirmed","script":"xpack/hooks/a.js"}}"#)
                .contains("not a moment")
        );
    }

    #[test]
    fn a_timeout_must_be_one_second_to_an_hour() {
        for bad in [0, 3601, u32::MAX] {
            let json =
                format!(r#"{{"install":{{"script":"xpack/hooks/a.js","timeoutSeconds":{bad}}}}}"#);
            assert!(refused(&json).contains("timeoutSeconds"), "{bad}");
        }
        for good in [1, 3600] {
            let json =
                format!(r#"{{"install":{{"script":"xpack/hooks/a.js","timeoutSeconds":{good}}}}}"#);
            hooks(&json).validate(&scripts()).unwrap();
        }
    }

    #[test]
    fn a_script_listed_twice_at_one_point_is_refused_and_at_two_points_is_not() {
        assert!(
            refused(
                r#"{"install":["xpack/hooks/a.js",{"script":"xpack/hooks/a.js","when":"after"}]}"#
            )
            .contains("listed twice")
        );
        hooks(r#"{"install":["xpack/hooks/a.js",{"script":"xpack/hooks/a.js","when":"before"}]}"#)
            .validate(&scripts())
            .unwrap();
    }

    #[test]
    fn a_script_must_be_a_js_file_of_the_payload_and_no_larger_than_a_mebibyte() {
        assert!(refused(r#"{"install":"bin/app"}"#).contains("not a .js file"));
        let upper = payload(&[("xpack/hooks/INSTALL.JS", 10)]);
        hooks(r#"{"install":"xpack/hooks/INSTALL.JS"}"#).validate(&upper).unwrap();
        assert!(
            refused(r#"{"install":"xpack/hooks/missing.js"}"#)
                .contains("not a file of the payload")
        );
        let big = payload(&[("xpack/hooks/big.js", MAX_SCRIPT_BYTES + 1)]);
        let error =
            hooks(r#"{"install":"xpack/hooks/big.js"}"#).validate(&big).unwrap_err().to_string();
        assert!(error.contains("bytes"), "{error}");
        let limit = payload(&[("xpack/hooks/big.js", MAX_SCRIPT_BYTES)]);
        hooks(r#"{"install":"xpack/hooks/big.js"}"#).validate(&limit).unwrap();
    }

    #[test]
    fn a_script_path_that_leaves_the_payload_is_refused() {
        for path in
            ["../outside.js", "/etc/hook.js", "xpack/../../x.js", "C:/x.js", ".xpack/hook.js"]
        {
            let json = format!(r#"{{"install":"{path}"}}"#);
            assert!(hooks(&json).validate(&scripts()).is_err(), "{path} was accepted");
        }
    }

    #[test]
    fn permissions_with_no_hook_are_refused() {
        assert!(refused(r#"{"permissions":{"user":{"exec":["git"]}}}"#).contains("no hooks"));
    }

    #[test]
    fn a_hook_for_one_user_may_never_ask_for_administrator_rights() {
        for program in [
            "sudo",
            "pkexec",
            "runas.exe",
            "/usr/bin/sudo",
            "osascript",
            "SU",
            "RUNAS.EXE",
            "Sudo.Exe",
            "runas.com",
            "runas.exe.",
            "C:\\Windows\\System32\\RunAs.Exe",
        ] {
            let json = serde_json::json!(
                {"install":"xpack/hooks/a.js","permissions":{"user":{"exec":[program]}}}
            )
            .to_string();
            assert!(refused(&json).contains("administrator rights"), "{program} was allowed");
            assert!(asks_for_administrator(program), "{program}");
        }
        for program in ["sudoku", "suspend", "runasroot", "git", "su-helper.exe"] {
            assert!(!asks_for_administrator(program), "{program}");
        }
        // The same programs are an administrator's own business.
        hooks(r#"{"install":"xpack/hooks/a.js","permissions":{"machine":{"exec":["runas.exe"]}}}"#)
            .validate(&scripts())
            .unwrap();
    }

    #[test]
    fn a_program_is_named_by_file_name_or_absolute_path() {
        hooks(r#"{"install":"xpack/hooks/a.js","permissions":{"user":{"exec":["systemctl","/usr/bin/launchctl","C:\\Windows\\System32\\sc.exe"]}}}"#)
            .validate(&scripts())
            .unwrap();
        for program in ["bin/tool", "..\\tool.exe", "", " "] {
            let json = serde_json::json!({"install":"xpack/hooks/a.js","permissions":{"user":{"exec":[program]}}}).to_string();
            assert!(hooks(&json).validate(&scripts()).is_err(), "{program:?} was allowed");
        }
    }

    #[test]
    fn a_place_for_one_user_is_only_under_their_own_home_or_temporary_directory() {
        hooks(r#"{"install":"xpack/hooks/a.js","permissions":{"user":{"write":["{home}/.config/systemd/user","{home}","{tempDir}/x"]}}}"#)
            .validate(&scripts())
            .unwrap();
        for place in [
            "/etc/systemd/system",
            "{programData}/x",
            "C:\\ProgramData\\x",
            "{home}/../other",
            "{home}x",
            "{unknown}/x",
            "relative/dir",
        ] {
            let json = serde_json::json!({"install":"xpack/hooks/a.js","permissions":{"user":{"write":[place]}}}).to_string();
            assert!(
                hooks(&json).validate(&scripts()).is_err(),
                "{place:?} was allowed for one user"
            );
        }
    }

    #[test]
    fn a_place_for_everyone_is_machine_wide_never_a_users_home() {
        hooks(r#"{"install":"xpack/hooks/a.js","permissions":{"machine":{"write":["/etc/systemd/system","{programData}/Example","C:\\ProgramData\\Example"]}}}"#)
            .validate(&scripts())
            .unwrap();
        for place in ["{home}/.config", "/etc/../root", "relative"] {
            let json = serde_json::json!({"install":"xpack/hooks/a.js","permissions":{"machine":{"write":[place]}}}).to_string();
            assert!(
                hooks(&json).validate(&scripts()).is_err(),
                "{place:?} was allowed for everyone"
            );
        }
    }

    // --- the record ---

    fn line(version: &str, at: &str, event: HookEvent) -> HookRecordLine {
        HookRecordLine {
            version: Version::parse(version).unwrap(),
            point: point(at),
            script: "xpack/hooks/a.js".into(),
            sha256: Sha256Digest::from_bytes([7; crate::SHA256_LEN]),
            event,
            at: 1_790_972_643,
            seconds: matches!(event, HookEvent::Succeeded | HookEvent::Failed).then_some(6),
        }
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn no_record_is_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        let record = HookRecord::read(&dir.path().join("hooks.jsonl")).unwrap();
        assert!(!record.has_run(&v("1.0.0"), point("install.after")));
    }

    #[test]
    fn the_record_says_what_ran_and_how_it_ended() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/hooks.jsonl");
        append(&path, &line("1.0.0", "install.after", HookEvent::Started)).unwrap();
        append(&path, &line("1.0.0", "install.after", HookEvent::Succeeded)).unwrap();
        append(&path, &line("1.1.0", "update.before", HookEvent::Started)).unwrap();
        append(&path, &line("1.1.0", "update.before", HookEvent::Failed)).unwrap();
        append(&path, &line("1.1.0", "update.after", HookEvent::Skipped)).unwrap();
        append(&path, &line("1.2.0", "update.after", HookEvent::Started)).unwrap();

        let record = HookRecord::read(&path).unwrap();
        assert_eq!(
            record.outcome(&v("1.0.0"), point("install.after")),
            Some(PointOutcome::Succeeded)
        );
        assert_eq!(record.outcome(&v("1.1.0"), point("update.before")), Some(PointOutcome::Failed));
        assert_eq!(record.outcome(&v("1.1.0"), point("update.after")), Some(PointOutcome::Failed));
        assert_eq!(
            record.outcome(&v("1.2.0"), point("update.after")),
            Some(PointOutcome::Interrupted)
        );
        assert!(
            record.has_run(&v("1.2.0"), point("update.after")),
            "an interrupted hook would run again"
        );
        assert!(!record.has_run(&v("1.2.0"), point("update.before")));
        assert!(
            !record.has_run(&v("1.0.0"), point("update.after")),
            "another version's run counted"
        );
        assert!(record.any_started(&v("1.1.0"), Operation::Update));
        assert!(!record.any_started(&v("1.0.0"), Operation::Update));
    }

    #[test]
    fn a_point_is_a_success_only_when_every_script_of_it_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.jsonl");
        let mut b = line("1.0.0", "install.after", HookEvent::Started);
        b.script = "xpack/hooks/b.js".into();
        append(&path, &line("1.0.0", "install.after", HookEvent::Started)).unwrap();
        append(&path, &line("1.0.0", "install.after", HookEvent::Succeeded)).unwrap();
        let record = HookRecord::read(&path).unwrap();
        let both = ["xpack/hooks/a.js", "xpack/hooks/b.js"];
        assert_eq!(
            record.outcome(&v("1.0.0"), point("install.after")),
            Some(PointOutcome::Succeeded)
        );
        assert!(record.all_succeeded(&v("1.0.0"), point("install.after"), ["xpack/hooks/a.js"]));
        assert!(!record.all_succeeded(&v("1.0.0"), point("install.after"), both), "b never ran");

        append(&path, &b).unwrap();
        b.event = HookEvent::Succeeded;
        append(&path, &b).unwrap();
        let record = HookRecord::read(&path).unwrap();
        assert!(record.all_succeeded(&v("1.0.0"), point("install.after"), both));
        assert!(!record.all_succeeded(&v("1.1.0"), point("install.after"), both));
    }

    #[test]
    fn one_hook_failing_at_a_point_fails_the_point_whatever_succeeds_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.jsonl");
        append(&path, &line("1.0.0", "install.after", HookEvent::Started)).unwrap();
        append(&path, &line("1.0.0", "install.after", HookEvent::Failed)).unwrap();
        append(&path, &line("1.0.0", "install.after", HookEvent::Started)).unwrap();
        append(&path, &line("1.0.0", "install.after", HookEvent::Succeeded)).unwrap();
        let record = HookRecord::read(&path).unwrap();
        assert_eq!(record.outcome(&v("1.0.0"), point("install.after")), Some(PointOutcome::Failed));
    }

    #[test]
    fn a_line_torn_by_a_power_cut_still_counts_as_a_start_when_it_names_its_point() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.jsonl");
        append(&path, &line("1.0.0", "install.after", HookEvent::Succeeded)).unwrap();
        let whole =
            serde_json::to_string(&line("1.1.0", "update.after", HookEvent::Started)).unwrap();
        let torn = &whole[..whole.find("\"script\"").unwrap() + 4];
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(torn.as_bytes())
            .unwrap();

        let record = HookRecord::read(&path).unwrap();

        assert!(
            record.has_run(&v("1.1.0"), point("update.after")),
            "a hook that may have run would run again"
        );
        assert_eq!(
            record.outcome(&v("1.1.0"), point("update.after")),
            Some(PointOutcome::Interrupted)
        );
        assert_eq!(
            record.outcome(&v("1.0.0"), point("install.after")),
            Some(PointOutcome::Succeeded)
        );
    }

    #[test]
    fn a_line_naming_nothing_that_can_be_made_out_stops_every_hook() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.jsonl");
        append(&path, &line("1.0.0", "install.after", HookEvent::Succeeded)).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"vers")
            .unwrap();
        let error = HookRecord::read(&path).unwrap_err().to_string();
        assert!(error.contains("no hook runs until it is repaired"), "{error}");
    }

    #[test]
    fn an_ordinary_request_says_nothing_of_a_plan_so_an_older_engine_still_reads_it() {
        let request = Request {
            script: PathBuf::from("/app/versions/1.0.0/a.js"),
            point: point("install.after"),
            cause: None,
            from_version: None,
            to_version: Some("1.0.0".into()),
            scope: Scope::User,
            application_dir: PathBuf::from("/app"),
            version_dir: PathBuf::from("/app/versions/1.0.0"),
            data_dir: PathBuf::from("/app/data"),
            log_dir: PathBuf::from("/app/state/logs"),
            temp_dir: PathBuf::from("/tmp/run"),
            home: None,
            program_data: None,
            environment: BTreeMap::new(),
            permissions: ScopePermissions::default(),
            timeout_seconds: 300,
            plan: None,
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(!json.contains("plan"), "{json}");
    }

    #[test]
    fn a_record_line_survives_being_written_and_read_back() {
        let written = line("1.1.0", "update.confirmed", HookEvent::Succeeded);
        let json = serde_json::to_string(&written).unwrap();
        assert!(json.contains("\"point\":\"update.confirmed\""), "{json}");
        assert_eq!(serde_json::from_str::<HookRecordLine>(&json).unwrap(), written);
    }
}
