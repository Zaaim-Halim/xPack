//! The individual commands.

pub(crate) mod activate;
pub(crate) mod inspect;
pub(crate) mod install;
pub(crate) mod keygen;
pub(crate) mod list;
pub(crate) mod pack;
pub(crate) mod prune;
pub(crate) mod recover;
pub(crate) mod rollback;
pub(crate) mod run;
pub(crate) mod uninstall;
pub(crate) mod update;
pub(crate) mod verify;

use std::path::PathBuf;
use std::process::ExitCode;

use xpack_core::{Error, InstallPaths, Result};
use xpack_platform::InstallLock;

/// Options shared by every command.
pub(crate) struct Context {
    /// Installation root override.
    pub(crate) root: Option<PathBuf>,
}

impl Context {
    /// Resolves the layout for an application.
    pub(crate) fn paths(&self, application_id: &str) -> Result<InstallPaths> {
        match &self.root {
            Some(root) => InstallPaths::new(root, application_id),
            None => InstallPaths::user_default(application_id),
        }
    }

    /// Takes the installation lock, reporting a busy installation clearly.
    pub(crate) fn lock(&self, application_id: &str) -> Result<InstallLock> {
        let paths = self.paths(application_id)?;
        InstallLock::acquire(&paths)
    }
}

/// The exit code for a command that did what was asked.
///
/// Returns a `Result` so command bodies can end with it directly, matching the
/// fallible paths around it, rather than splitting into two shapes.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn success() -> Result<ExitCode> {
    Ok(ExitCode::SUCCESS)
}

/// Reads a public key from a file, or parses it as hex.
///
/// Accepting both means a key can be pasted from a release page without first
/// being saved to a file, which is the common case for a first install.
pub(crate) fn public_key(value: &str) -> Result<xpack_security::PublicKey> {
    let path = std::path::Path::new(value);
    if path.exists() {
        return xpack_security::PublicKey::load(path);
    }
    xpack_security::PublicKey::parse_hex(value).map_err(|e| {
        // A key can fail to parse for two very different reasons, and the
        // exit code must tell them apart. "That is not hex" is a typo. "That
        // is a low-order point" means the key would accept a signature from
        // anyone — a security failure a script must never retry.
        if e.is_integrity_failure() {
            return e;
        }
        Error::invalid(
            "key",
            format!("{value:?} is neither a readable file nor a 64-character hex key"),
        )
    })
}
