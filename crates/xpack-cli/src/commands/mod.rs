//! The individual commands.

pub(crate) mod activate;
pub(crate) mod autoupdate;
pub(crate) mod delta;
pub(crate) mod hooks;
pub(crate) mod hooks_gate;
pub(crate) mod hooks_test;
pub(crate) mod index;
pub(crate) mod inspect;
pub(crate) mod install;
pub(crate) mod installer;
pub(crate) mod keygen;
pub(crate) mod list;
pub(crate) mod pack;
pub(crate) mod prune;
pub(crate) mod recover;
pub(crate) mod rollback;
pub(crate) mod run;
pub(crate) mod sealing;
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

/// A flag a running hook is stopped by, set when the person at the terminal
/// presses Ctrl-C.
///
/// `xpack-hook` runs in a process group of its own, so a terminal's interrupt
/// no longer reaches it; without this, `xpack` would die and the hook run on
/// to its timeout. The first Ctrl-C stops the hook, which undoes the
/// operation as for any failed hook; a second quits at once. Installed only
/// by an operation that runs hooks, so everything else keeps the ordinary
/// Ctrl-C.
pub(crate) fn cancel_on_interrupt() -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};
    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    Arc::clone(FLAG.get_or_init(|| {
        let flag = Arc::new(AtomicBool::new(false));
        let set = Arc::clone(&flag);
        let installed = ctrlc::set_handler(move || {
            if set.swap(true, Ordering::SeqCst) {
                std::process::exit(130);
            }
            xpack_core::errln!(
                "stopping the running hook; what it began is undone. Press Ctrl-C again to quit \
                 at once."
            );
        });
        if let Err(error) = installed {
            tracing::warn!(%error, "Ctrl-C will not stop a running hook");
        }
        flag
    }))
}

/// The `xpack-hook` an operation on an installation runs hooks with: the
/// installation's own, which serves the hook interface it was installed
/// for, or the one beside `xpack` when it has none.
pub(crate) fn hook_engine_for(paths: &InstallPaths) -> Option<PathBuf> {
    Some(paths.hook_engine_file()).filter(|engine| engine.is_file()).or_else(default_hook_engine)
}

/// The `xpack-launcher` binary sitting beside this executable, if there is one.
///
/// Shared by the commands that can create an installation. Both default to it
/// so that an installation always ends up with an entry point, however it came
/// to exist — an update that happens to be a first install should not produce
/// something the user cannot start.
pub(crate) fn default_launcher() -> Option<std::path::PathBuf> {
    sibling_binary("xpack-launcher")
}

/// The `xpack-launcherw` binary sitting beside this executable, if there is one.
///
/// The windowed build, which only Windows distinguishes. Callers decide
/// whether to place it; this only finds it.
pub(crate) fn default_gui_launcher() -> Option<std::path::PathBuf> {
    sibling_binary("xpack-launcherw")
}

/// The `xpack-updater` binary sitting beside this executable, if there is one.
pub(crate) fn default_updater() -> Option<std::path::PathBuf> {
    sibling_binary("xpack-updater")
}

/// The `xpack-notify` binary sitting beside this executable, if there is one.
///
/// Offered to every install; placed only into installations whose package asks
/// for a prompt, which the installer decides from the manifest.
pub(crate) fn default_notifier() -> Option<std::path::PathBuf> {
    sibling_binary("xpack-notify")
}

/// The `xpack-hook` binary sitting beside this executable, if there is one.
///
/// Offered to every install; placed in every installation, whether or not its
/// package has hooks, so that one can receive them in an update.
pub(crate) fn default_hook_engine() -> Option<std::path::PathBuf> {
    sibling_binary("xpack-hook")
}

/// The `xpack-uninstaller` binary sitting beside this executable, if there is one.
pub(crate) fn default_uninstaller() -> Option<std::path::PathBuf> {
    sibling_binary("xpack-uninstaller")
}

/// Finds a named executable next to this one, or explains that it is missing.
///
/// Used where the binary is required rather than optional: building an
/// installer without the launcher would produce an application with no entry
/// point, and the failure has to name what is missing and where it was sought.
pub(crate) fn sibling_binary_required(name: &str) -> xpack_core::Result<std::path::PathBuf> {
    sibling_binary(name).ok_or_else(|| {
        xpack_core::Error::invalid(
            "binary",
            format!(
                "{name} was not found beside this executable; build the workspace first, or \
                 pass it explicitly"
            ),
        )
    })
}

/// Finds a named executable next to this one.
fn sibling_binary(name: &str) -> Option<std::path::PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let candidate = executable.parent()?.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate)
}
