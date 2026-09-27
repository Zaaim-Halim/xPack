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

    fn inspect_everyone(&self) -> Option<Inspection> {
        if self.ui().all_users == xpack_installer_ui::AllUsers::Never {
            return None;
        }
        let application = &self.manifest().application;
        let target =
            xpack_install::integration::machine_application_dir(&application.id, &application.name)
                .ok()?;
        let paths = xpack_core::InstallPaths::named(&target, &application.id).ok()?;
        let verdict = Ok(xpack_install::inspect(&paths, &application.version));
        Some(Inspection { target, verdict })
    }

    fn install(&self, choices: &Choices, progress: &dyn ProgressReporter) -> Result<Installed> {
        if choices.everyone && !xpack_platform::is_elevated() {
            return self.install_elevated(choices);
        }
        let request = Request {
            root: choices.root.clone(),
            desktop_entry: choices.desktop_entry,
            desktop_shortcut: choices.desktop_shortcut,
            command: choices.command,
            scope: if choices.everyone {
                xpack_core::InstallScope::Machine
            } else {
                xpack_core::InstallScope::User
            },
        };
        let outcome = self.install_into(&request, progress)?;
        Ok(Installed {
            directory: outcome.root,
            version: outcome.version,
            shortcut_added: matches!(outcome.desktop, DesktopOutcome::Done(_)),
            command: outcome.command_names.first().cloned(),
            command_off_path: outcome.command_off_path,
        })
    }

    fn launch(&self, root: &Path) -> Result<()> {
        // An installation for everyone is reported as its own directory,
        // named after the application; one for the person installing, as the
        // root it was made in.
        let paths = if root.join("state").join("state.json").is_file() {
            xpack_core::InstallPaths::open(root)
        } else {
            self.paths(root)?
        };
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
    /// Installs for everyone by running this installer again with
    /// administrator rights, asked for the way the platform asks, and waits.
    ///
    /// The installer run that way is this very file, told what the person
    /// chose on its command line, installing silently. What it did is then
    /// read from the installation rather than taken on its word.
    fn install_elevated(&self, choices: &Choices) -> Result<Installed> {
        let program = std::env::current_exe()
            .map_err(|e| Error::invalid("installer", format!("cannot locate itself: {e}")))?;
        let mut arguments: Vec<std::ffi::OsString> = vec!["--silent".into(), "--all-users".into()];
        if choices.desktop_entry == Some(false) {
            arguments.push("--no-shortcut".into());
        }
        if !choices.desktop_shortcut {
            arguments.push("--no-desktop-shortcut".into());
        }
        if choices.command == Some(false) {
            arguments.push("--no-path".into());
        }
        // A locked installer hands its elevated self the key it was opened
        // with, never the password, in a file only this user can read, which
        // the elevated run deletes as it reads it.
        let key_file = match self.seal_key() {
            Some(key) => {
                let file = hand_over_key(key)?;
                arguments.push("--seal-key-file".into());
                arguments.push(file.clone().into_os_string());
                Some(file)
            }
            None => None,
        };

        let ran = xpack_platform::run_elevated(&program, &arguments);
        if let Some(file) = &key_file {
            let _ = std::fs::remove_file(file);
        }
        match ran? {
            xpack_platform::Elevated::Declined => Err(Error::invalid(
                "installation",
                "administrator rights were not given, so nothing was installed",
            )),
            xpack_platform::Elevated::Exited(0) => self.installed_for_everyone(),
            xpack_platform::Elevated::Exited(code) => Err(Error::invalid(
                "installation",
                format!("installing for everyone failed (exit code {code})"),
            )),
        }
    }

    /// What an installation for everyone holds now, read from it.
    fn installed_for_everyone(&self) -> Result<Installed> {
        let application = &self.manifest().application;
        let directory = xpack_install::integration::machine_application_dir(
            &application.id,
            &application.name,
        )?;
        let paths = xpack_core::InstallPaths::open(&directory);
        let state = InstallState::load(&paths.state_file())?.value;
        if state.current_version.as_ref() != Some(&application.version) {
            return Err(Error::invalid(
                "installation",
                format!("{} was not installed for everyone", application.version),
            ));
        }
        Ok(Installed {
            directory,
            version: application.version.clone(),
            shortcut_added: self.manifest().desktop.shortcut
                && !paths.desktop_preference_file().is_file(),
            command: self.manifest().command.as_ref().map(|command| command.name.clone()),
            command_off_path: None,
        })
    }

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

/// Writes `key` to a new file only this user can read, for the elevated run.
fn hand_over_key(key: &xpack_security::seal::SealKey) -> Result<std::path::PathBuf> {
    use std::io::Write;

    let mut name = [0u8; 12];
    getrandom::fill(&mut name)
        .map_err(|e| Error::invalid("installer", format!("no randomness: {e}")))?;
    let file = std::env::temp_dir().join(format!("xpack-seal-key-{}", hex::encode(name)));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut out = options.open(&file).map_err(|e| Error::io(&file, e))?;
    out.write_all(key.to_hex().as_bytes()).map_err(|e| Error::io(&file, e))?;
    Ok(file)
}
