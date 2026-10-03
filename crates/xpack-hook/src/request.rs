//! What the program that runs a hook is told, on standard input.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use xpack_core::hooks::{HookPoint, Scope, ScopePermissions};

/// One hook to run, and everything it is given.
///
/// Sent by the installer, launcher or uninstaller as one JSON document on
/// standard input, read whole before the script starts: the script itself
/// gets no input.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    /// The script, an absolute path inside [`Self::version_dir`].
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
        if !self.script.starts_with(&self.version_dir) {
            return Err(format!(
                "the script {} is not in its version's directory",
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

/// The variables a program a hook runs needs from this process's own
/// environment, and nothing else of it.
///
/// Built from a list of what programs need rather than from everything minus
/// what is known to be secret, so a password the installer was given, under
/// whatever name, never reaches a hook's program.
const PASSED_THROUGH: [&str; 17] = [
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "USERPROFILE",
    "USERNAME",
    "TMP",
    "TEMP",
    "SystemRoot",
    "windir",
    "ComSpec",
    "PATHEXT",
];

/// The environment a hook's programs run with: what they need from this one,
/// the application's launch environment, and nothing of xPack's own.
pub(crate) fn program_environment(request: &Request) -> BTreeMap<String, String> {
    let mut environment: BTreeMap<String, String> = std::env::vars()
        .filter(|(name, _)| PASSED_THROUGH.iter().any(|allowed| allowed.eq_ignore_ascii_case(name)))
        .collect();
    for (name, value) in &request.environment {
        if !steers_programs(name) {
            environment.insert(name.clone(), value.clone());
        }
    }
    environment
}

/// A variable a hook's programs never get from a hook or a package: one that
/// steers xPack or carries one of its secrets, or one that changes which code
/// a program runs rather than what it does.
///
/// `PATH` among them: the program itself is found before it starts, but
/// Windows also looks along `PATH` for the libraries a program loads, and a
/// program looks along it for whatever it starts in turn.
pub(crate) fn steers_programs(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    name == "PATH" || ["XPACK_", "LD_", "DYLD_"].iter().any(|prefix| name.starts_with(prefix))
}
