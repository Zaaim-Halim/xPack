//! Restarting a macOS application by opening its bundle again.
//!
//! macOS gives a bundle's name and icon only to the first application that
//! starts after the bundle is opened. The launcher the bundle opens runs the
//! application as its child, so a second start from that same launcher shows
//! in the Dock under the program's own name ("java") with a generic icon. A
//! restart after an update therefore opens the bundle again, as a new launch,
//! and this launcher steps aside.
//!
//! Only when the launcher was opened through its bundle. Started from a
//! terminal or from a command on the `PATH`, the application restarts in this
//! process as before, and keeps its terminal.
//!
//! The new launch is told it is a restart, so it offers none of its own: one
//! restart per start, the same rule as a restart in place.

use std::ffi::OsStr;

use xpack_core::InstallPaths;

/// Set on the launch that reopens the bundle, to the application's id.
///
/// The id rather than a flag, because the application inherits it and may
/// start other xPack applications, whose launchers must not take it as theirs.
pub(crate) const RESTARTED_ENV: &str = "XPACK_RESTARTED";

/// Whether this launch is the restart another launcher asked for.
pub(crate) fn is_a_restart(paths: &InstallPaths) -> bool {
    restarted_for(paths, std::env::var_os(RESTARTED_ENV).as_deref())
}

fn restarted_for(paths: &InstallPaths, value: Option<&OsStr>) -> bool {
    match (value, paths.application_id()) {
        (Some(value), Some(application)) => value == application,
        _ => false,
    }
}

/// Opens the bundle this launcher was opened through, to start the
/// application again. `true` when it did, and this launcher's part is over.
///
/// `false` leaves the restart to the caller, in place: not opened through a
/// bundle, or `open` failed. An application that comes back under the wrong
/// name is better than one that does not come back.
#[cfg(target_os = "macos")]
pub(crate) fn through_bundle(paths: &InstallPaths, arguments: &[String]) -> bool {
    use std::process::Stdio;

    let told = std::env::var_os(xpack_install::integration::BUNDLE_ENV);
    let (Some(bundle), Some(application)) =
        (own_bundle(paths, told.as_deref()), paths.application_id())
    else {
        return false;
    };

    let mut command = reopen_command(&bundle, application, arguments);
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    match command.status() {
        Ok(status) if status.success() => {
            tracing::info!(bundle = %bundle.display(), "restarting through the application's bundle");
            true
        }
        Ok(status) => {
            tracing::warn!(%status, bundle = %bundle.display(), "could not open the bundle; restarting in place");
            false
        }
        Err(error) => {
            tracing::warn!(%error, bundle = %bundle.display(), "could not open the bundle; restarting in place");
            false
        }
    }
}

/// Nowhere else does a restart need to go through anything but this process.
#[cfg(not(target_os = "macos"))]
pub(crate) fn through_bundle(paths: &InstallPaths, arguments: &[String]) -> bool {
    let _ = (paths, arguments);
    false
}

/// The bundle this launcher was told it was opened through, if that bundle
/// starts this installation's launcher.
///
/// The variable is inherited by everything the application starts, including
/// other xPack applications' launchers, so what it names is checked rather
/// than believed.
#[cfg(target_os = "macos")]
fn own_bundle(paths: &InstallPaths, told: Option<&OsStr>) -> Option<std::path::PathBuf> {
    let bundle = std::fs::canonicalize(told?).ok()?;
    let launcher = xpack_install::integration::bundle_launcher(&bundle)?;
    let starts = std::fs::canonicalize(launcher.parent()?).ok()?;
    (starts == std::fs::canonicalize(paths.root()).ok()?).then_some(bundle)
}

/// `open -n --env XPACK_RESTARTED=<id> <bundle> --args <arguments>`.
///
/// `-n` because the bundle's process, this launcher, is still running, and
/// without it macOS would only bring that process to the front.
#[cfg(target_os = "macos")]
fn reopen_command(
    bundle: &std::path::Path,
    application: &str,
    arguments: &[String],
) -> std::process::Command {
    let mut command = std::process::Command::new("/usr/bin/open");
    command.arg("-n").arg("--env").arg(format!("{RESTARTED_ENV}={application}")).arg(bundle);
    if !arguments.is_empty() {
        command.arg("--args").args(arguments);
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &std::path::Path) -> InstallPaths {
        InstallPaths::new(dir, "com.example.app").unwrap()
    }

    #[test]
    fn a_restart_is_recognised_only_for_its_own_application() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());

        assert!(restarted_for(&paths, Some(OsStr::new("com.example.app"))));
        assert!(!restarted_for(&paths, None), "an ordinary start");
        assert!(
            !restarted_for(&paths, Some(OsStr::new("com.example.other"))),
            "inherited from another application"
        );
        assert!(!restarted_for(&paths, Some(OsStr::new(""))));
    }

    #[cfg(target_os = "macos")]
    mod macos {
        use std::path::Path;

        use super::*;

        /// Writes a bundle whose script starts `launcher`, the way an install does.
        fn bundle(dir: &Path, name: &str, launcher: &Path) -> std::path::PathBuf {
            let bundle = dir.join(format!("{name}.app"));
            let macos = bundle.join("Contents").join("MacOS");
            std::fs::create_dir_all(&macos).unwrap();
            std::fs::write(
                macos.join(name),
                format!("#!/bin/sh\nexec '{}' \"$@\"\n", launcher.display()),
            )
            .unwrap();
            bundle
        }

        #[test]
        fn the_bundle_that_starts_this_installation_is_reopened() {
            let dir = tempfile::tempdir().unwrap();
            let paths = paths(dir.path());
            std::fs::create_dir_all(paths.root()).unwrap();
            let bundle = bundle(dir.path(), "App", &paths.root().join("xpack-launcher"));

            assert_eq!(
                own_bundle(&paths, Some(bundle.as_os_str())),
                Some(std::fs::canonicalize(&bundle).unwrap())
            );
        }

        #[test]
        fn a_bundle_of_another_installation_is_not_reopened() {
            // Another xPack application started from this one inherits the
            // variable naming this one's bundle; its launcher must not reopen it.
            let dir = tempfile::tempdir().unwrap();
            let paths = paths(dir.path());
            let other = InstallPaths::new(dir.path(), "com.example.other").unwrap();
            std::fs::create_dir_all(paths.root()).unwrap();
            std::fs::create_dir_all(other.root()).unwrap();
            let bundle = bundle(dir.path(), "Other", &other.root().join("xpack-launcher"));

            assert_eq!(own_bundle(&paths, Some(bundle.as_os_str())), None);
        }

        #[test]
        fn a_launcher_not_opened_through_a_bundle_reopens_nothing() {
            let dir = tempfile::tempdir().unwrap();
            let paths = paths(dir.path());
            std::fs::create_dir_all(paths.root()).unwrap();

            assert_eq!(own_bundle(&paths, None), None, "started from a terminal");
            assert_eq!(
                own_bundle(&paths, Some(dir.path().join("missing.app").as_os_str())),
                None,
                "a bundle that is not there"
            );
            assert!(!through_bundle(&paths, &[]), "nothing to open, so restart in place");
        }

        #[test]
        fn the_bundle_is_opened_as_a_new_instance_marked_as_a_restart() {
            let command = reopen_command(
                Path::new("/Users/u/Applications/My App.app"),
                "com.example.app",
                &["--flag".into(), "a file".into()],
            );
            assert_eq!(command.get_program(), "/usr/bin/open");
            let arguments: Vec<_> = command.get_args().collect();
            assert_eq!(
                arguments,
                [
                    "-n",
                    "--env",
                    "XPACK_RESTARTED=com.example.app",
                    "/Users/u/Applications/My App.app",
                    "--args",
                    "--flag",
                    "a file"
                ]
            );
        }

        #[test]
        fn with_no_arguments_none_are_passed_on() {
            let command = reopen_command(Path::new("/A.app"), "com.example.app", &[]);
            assert!(!command.get_args().any(|argument| argument == "--args"));
        }
    }
}
