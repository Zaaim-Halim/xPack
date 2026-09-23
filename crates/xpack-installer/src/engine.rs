//! The installer, as the installation wizard sees it.
//!
//! Every answer here is the console installer's own: the same inspection,
//! the same install call, the same request with the same defaults. The wizard
//! only decides when to ask.

use std::path::Path;
use std::process::{Command, Stdio};

use xpack_core::{BinaryNames, Error, InstallState, ProgressReporter, Result};
use xpack_install::{DesktopOutcome, Existing};
use xpack_installer_ui::{Choices, Engine, Inspection, Installed, RootProblem};

use crate::{Request, VerifiedPayload, launcher_to_report};

impl Engine for VerifiedPayload {
    fn inspect(&self, root: &Path) -> Inspection {
        let target = root.join(&self.manifest().application.id);
        let verdict = self.check_root(root).map(|()| {
            VerifiedPayload::inspect(self, root)
                .unwrap_or_else(|error| Existing::Unreadable(error.to_string()))
        });
        Inspection { target, verdict }
    }

    fn install(&self, choices: &Choices, progress: &dyn ProgressReporter) -> Result<Installed> {
        let request = Request {
            root: choices.root.clone(),
            desktop_entry: choices.desktop_entry,
            command: choices.command,
        };
        let outcome = self.install_into(&request, progress)?;
        Ok(Installed {
            directory: outcome.root,
            version: outcome.version,
            shortcut_added: matches!(outcome.desktop, DesktopOutcome::Done(_)),
            command: outcome.command_name,
            command_off_path: outcome.command_off_path,
        })
    }

    fn launch(&self, root: &Path) -> Result<()> {
        let paths = self.paths(root)?;
        // Read without the lock, as looking always is: the names only decide
        // which file to start, and the state is replaced atomically.
        let names = InstallState::load(&paths.state_file())
            .map_or(BinaryNames::Xpack, |loaded| loaded.value.binary_names());
        let launcher = launcher_to_report(&paths, &names);
        if !launcher.is_file() {
            return Err(Error::invalid(
                "launch",
                format!("{} is not installed", launcher.display()),
            ));
        }

        // Through the launcher, as a menu entry would, so the application's
        // health check and probation run exactly as they do on every start.
        // Its working directory is the installation, never this installer's
        // temporary directory, which is deleted when the installer exits.
        let mut command = Command::new(&launcher);
        command
            .current_dir(paths.root())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach(&mut command);
        command.spawn().map(drop).map_err(|e| Error::io(&launcher, e))
    }
}

impl VerifiedPayload {
    /// Why `root` cannot be installed into at all, before looking inside it.
    ///
    /// Nothing is created to find out. Whether a directory can really be
    /// written is only certain by writing to it, which is exactly what looking
    /// must not do; a folder that passes here and still refuses is reported by
    /// the install itself.
    fn check_root(&self, root: &Path) -> std::result::Result<(), RootProblem> {
        if !root.is_absolute() {
            return Err(RootProblem::NotAbsolute);
        }

        let manifest = self.manifest();
        let version_dir = root
            .join(&manifest.application.id)
            .join("versions")
            .join(manifest.application.version.to_directory_name());
        if xpack_core::ensure_launch_paths_fit(
            &version_dir,
            &manifest.launch,
            xpack_core::host_launch_path_limit(),
        )
        .is_err()
        {
            return Err(RootProblem::TooDeep);
        }

        // The nearest part of the path that exists is where anything would be
        // created first.
        let mut existing = root;
        while !existing.exists() {
            match existing.parent() {
                Some(parent) => existing = parent,
                None => return Err(RootProblem::NotWritable),
            }
        }
        match std::fs::metadata(existing) {
            Ok(metadata) if metadata.is_dir() && !metadata.permissions().readonly() => Ok(()),
            _ => Err(RootProblem::NotWritable),
        }
    }
}

/// Lets the application outlive the installer that started it.
#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    // Not attached to this process's console, if it has one, and not in its
    // process group, so closing the installer does not close the application.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// Lets the application outlive the installer that started it.
///
/// On Unix a child already does: it is re-parented when the installer exits.
#[cfg(not(windows))]
fn detach(_command: &mut Command) {}
