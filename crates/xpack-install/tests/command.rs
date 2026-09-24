//! The command a terminal starts an application by, against real installs.
//!
//! Every place it goes is redirected: `~/.local/bin` into a temporary
//! directory, and on Windows the `PATH` into a scratch registry key that is
//! deleted afterwards. A test run never touches the developer's own commands
//! or a build machine's real `PATH`.

mod common;

use std::path::PathBuf;

use xpack_core::InstallPaths;
use xpack_install::integration::command::CommandRoots;
use xpack_install::{DesktopOutcome, InstallOptions, Installed, Installer, TrustDecision};
use xpack_platform::InstallLock;
use xpack_security::KeyPair;

struct World {
    dir: tempfile::TempDir,
    key: KeyPair,
    paths: InstallPaths,
    roots: CommandRoots,
    launcher: PathBuf,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let key = KeyPair::generate().unwrap();
        let paths = common::install_paths(&dir.path().join("root"));
        // Unique per test, so tests running side by side never share one.
        let unique = dir.path().file_name().unwrap().to_string_lossy().into_owned();
        let roots = CommandRoots {
            bin: dir.path().join("home/.local/bin"),
            environment_key: format!(r"Software\xpack-tests\{unique}"),
        };
        // Records its arguments, so a test can see them arrive.
        let launcher = dir.path().join("launcher");
        std::fs::write(&launcher, "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$XPACK_TEST_OUT\"\n")
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self { dir, key, paths, roots, launcher }
    }

    fn options(&self, choice: Option<bool>) -> InstallOptions {
        InstallOptions {
            activate: true,
            launcher: Some(self.launcher.clone()),
            command: choice,
            command_roots: Some(self.roots.clone()),
            ..Default::default()
        }
    }

    fn install(
        &self,
        version: &str,
        command: Option<&str>,
        options: &InstallOptions,
    ) -> xpack_core::Result<Installed> {
        let lock = InstallLock::acquire(&self.paths).unwrap();
        let source = self.dir.path().join(format!("build-{version}"));
        std::fs::create_dir_all(&source).unwrap();
        let package = common::build_package_commanding(&source, &self.key, version, command);
        let mut verified = xpack_install::open_and_verify(
            &package,
            &lock,
            &TrustDecision::Explicit(self.key.public()),
        )?;
        Installer::new(&lock).install(&mut verified, options)
    }

    fn uninstall(&self) -> xpack_install::installer::Removal {
        let lock = InstallLock::acquire(&self.paths).unwrap();
        xpack_install::uninstall_into(lock, None, Some(&self.roots)).unwrap()
    }
}

fn created(outcome: &DesktopOutcome) -> Vec<PathBuf> {
    match outcome {
        DesktopOutcome::Done(paths) => paths.clone(),
        other => panic!("expected the command to be put in place, got {other:?}"),
    }
}

#[test]
fn a_package_without_a_command_touches_nothing() {
    let world = World::new();
    let installed = world.install("1.0.0", None, &world.options(None)).unwrap();
    assert_eq!(installed.command, DesktopOutcome::NotRequested);
    assert!(!world.roots.bin.exists(), "a command directory was created for nothing");
}

#[test]
fn without_a_launcher_the_command_is_left_out_and_the_install_still_succeeds() {
    // A command that runs a missing launcher would fail every time it is typed.
    let world = World::new();
    let options = InstallOptions { launcher: None, ..world.options(None) };
    let installed = world.install("1.0.0", Some("mytool"), &options).unwrap();
    assert!(matches!(installed.command, DesktopOutcome::Failed(_)), "{:?}", installed.command);
    assert!(!world.roots.bin.join("mytool").exists());
}

#[test]
fn declining_after_the_first_install_is_refused() {
    // The installation's updater may predate the record and put it back.
    let world = World::new();
    world.install("1.0.0", Some("mytool"), &world.options(None)).unwrap();
    let err = world.install("1.1.0", Some("mytool"), &world.options(Some(false))).unwrap_err();
    assert!(err.to_string().contains("first installation"), "{err}");
}

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn script(world: &World, name: &str) -> PathBuf {
        world.roots.bin.join(name)
    }

    #[test]
    fn a_command_goes_where_a_terminal_looks_and_passes_every_argument_on() {
        let world = World::new();
        let installed = world.install("1.0.0", Some("mytool"), &world.options(None)).unwrap();
        let path = script(&world, "mytool");
        assert_eq!(created(&installed.command), vec![path.clone()]);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o755);

        // Run as a terminal would: arguments with spaces and quotes arrive
        // exactly as typed, one per argument.
        let out = world.dir.path().join("args");
        let mut command = std::process::Command::new(&path);
        command.args(["a b", "it's", "--flag"]).env("XPACK_TEST_OUT", &out);
        // Linux refuses to execute a file some process still holds open for
        // writing, and a child forked by another test thread while the script
        // was being written keeps a copy of that handle until it execs. That
        // lasts milliseconds, so it is retried rather than reported.
        let mut attempts = 0;
        let status = loop {
            match command.status() {
                Err(error)
                    if error.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 100 =>
                {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                result => break result.unwrap(),
            }
        };
        assert!(status.success());
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "a b\nit's\n--flag\n");
    }

    #[test]
    fn someone_elses_command_is_never_replaced_nor_removed() {
        let world = World::new();
        std::fs::create_dir_all(&world.roots.bin).unwrap();
        let theirs = script(&world, "mytool");
        std::fs::write(&theirs, "#!/bin/sh\necho theirs\n").unwrap();

        let installed = world.install("1.0.0", Some("mytool"), &world.options(None)).unwrap();
        assert!(matches!(installed.command, DesktopOutcome::Failed(_)), "{:?}", installed.command);
        assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "#!/bin/sh\necho theirs\n");

        world.uninstall();
        assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "#!/bin/sh\necho theirs\n");
    }

    #[test]
    fn a_link_or_another_applications_script_is_someone_elses_too() {
        let world = World::new();
        std::fs::create_dir_all(&world.roots.bin).unwrap();

        // A link, even one pointing at our own launcher: we did not make it.
        let link = script(&world, "mytool");
        std::os::unix::fs::symlink(&world.launcher, &link).unwrap();
        let installed = world.install("1.0.0", Some("mytool"), &world.options(None)).unwrap();
        assert!(matches!(installed.command, DesktopOutcome::Failed(_)));
        assert_eq!(std::fs::read_link(&link).unwrap(), world.launcher);

        // Another application's script, marked as theirs.
        std::fs::remove_file(&link).unwrap();
        let other = format!(
            "#!/bin/sh\n{}\nexec /elsewhere \"$@\"\n",
            xpack_install::integration::command::ownership_mark("com.example.other")
        );
        std::fs::write(&link, &other).unwrap();
        let world_update = world.install("1.1.0", Some("mytool"), &world.options(None)).unwrap();
        assert!(matches!(world_update.command, DesktopOutcome::Failed(_)));
        world.uninstall();
        assert_eq!(std::fs::read_to_string(&link).unwrap(), other);
    }

    #[test]
    fn a_declined_command_stays_declined_through_updates() {
        let world = World::new();
        let first = world.install("1.0.0", Some("mytool"), &world.options(Some(false))).unwrap();
        assert_eq!(first.command, DesktopOutcome::NotRequested);
        let update = world.install("1.1.0", Some("mytool"), &world.options(None)).unwrap();
        assert_eq!(update.command, DesktopOutcome::NotRequested);
        assert!(!script(&world, "mytool").exists());
    }

    #[test]
    fn an_update_that_renames_the_command_moves_it() {
        let world = World::new();
        world.install("1.0.0", Some("mytool"), &world.options(None)).unwrap();
        world.install("1.1.0", Some("newtool"), &world.options(None)).unwrap();
        assert!(!script(&world, "mytool").exists(), "the old name was left behind");
        assert!(script(&world, "newtool").is_file());
    }

    #[test]
    fn an_update_that_drops_the_command_takes_it_away() {
        let world = World::new();
        world.install("1.0.0", Some("mytool"), &world.options(None)).unwrap();
        let update = world.install("1.1.0", None, &world.options(None)).unwrap();
        assert_eq!(update.command, DesktopOutcome::NotRequested);
        assert!(!script(&world, "mytool").exists());
    }

    #[test]
    fn an_update_keeps_the_command_it_already_has() {
        let world = World::new();
        world.install("1.0.0", Some("mytool"), &world.options(None)).unwrap();
        let update = world.install("1.1.0", Some("mytool"), &world.options(None)).unwrap();
        assert_eq!(created(&update.command), vec![script(&world, "mytool")]);
    }

    #[test]
    fn uninstalling_one_copy_leaves_the_command_another_copy_now_owns() {
        // The same application installed twice, in two roots, sharing one
        // `~/.local/bin`. The second install points the command at itself;
        // removing the first must not take the second's command with it.
        let first = World::new();
        let second_paths = common::install_paths(&first.dir.path().join("elsewhere"));
        first.install("1.0.0", Some("mytool"), &first.options(None)).unwrap();

        let lock = InstallLock::acquire(&second_paths).unwrap();
        let source = first.dir.path().join("build-second");
        std::fs::create_dir_all(&source).unwrap();
        let package =
            common::build_package_commanding(&source, &first.key, "1.0.0", Some("mytool"));
        let mut verified = xpack_install::open_and_verify(
            &package,
            &lock,
            &TrustDecision::Explicit(first.key.public()),
        )
        .unwrap();
        Installer::new(&lock).install(&mut verified, &first.options(None)).unwrap();
        drop(lock);
        let pointed_at_second = std::fs::read_to_string(script(&first, "mytool")).unwrap();

        first.uninstall();
        assert_eq!(
            std::fs::read_to_string(script(&first, "mytool")).ok().as_deref(),
            Some(pointed_at_second.as_str()),
            "removing one copy took away the command the other copy owns"
        );
    }

    #[test]
    fn uninstalling_takes_the_command_away() {
        let world = World::new();
        world.install("1.0.0", Some("mytool"), &world.options(None)).unwrap();
        let removal = world.uninstall();
        assert!(removal.command.is_done(), "{:?}", removal.command);
        assert!(!script(&world, "mytool").exists());
    }
}

#[cfg(windows)]
mod windows {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, RegType};

    use super::*;

    /// The scratch `Path`, as its raw type and text.
    fn path_value(world: &World) -> Option<(RegType, String)> {
        use winreg::types::FromRegValue;
        let key =
            RegKey::predef(HKEY_CURRENT_USER).open_subkey(&world.roots.environment_key).ok()?;
        let raw = key.get_raw_value("Path").ok()?;
        Some((raw.vtype.clone(), String::from_reg_value(&raw).unwrap()))
    }

    fn seed_path(world: &World, value: &str) {
        let (key, _) =
            RegKey::predef(HKEY_CURRENT_USER).create_subkey(&world.roots.environment_key).unwrap();
        let bytes: Vec<u8> =
            value.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect();
        key.set_raw_value(
            "Path",
            &winreg::RegValue { bytes: bytes.into(), vtype: RegType::REG_EXPAND_SZ },
        )
        .unwrap();
    }

    /// Deletes the scratch key, whatever the test did.
    struct Cleanup(String);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(&self.0);
        }
    }

    #[test]
    fn the_command_directory_joins_the_users_path_and_leaves_with_it() {
        let world = World::new();
        let _cleanup = Cleanup(world.roots.environment_key.clone());
        let before = r"%USERPROFILE%\bin;C:\Tools";
        seed_path(&world, before);

        let installed = world.install("1.0.0", Some("mytool"), &world.options(None)).unwrap();
        let copy = world.paths.command_dir().join("mytool.exe");
        assert_eq!(created(&installed.command), vec![std::path::absolute(&copy).unwrap()]);
        assert!(copy.is_file());

        let (kind, value) = path_value(&world).unwrap();
        assert_eq!(kind, RegType::REG_EXPAND_SZ, "the value's type was changed");
        let dir = std::path::absolute(world.paths.command_dir()).unwrap();
        assert_eq!(value, format!("{before};{}", dir.display()));

        // Installed again: not added a second time.
        world.install("1.1.0", Some("mytool"), &world.options(None)).unwrap();
        assert_eq!(path_value(&world).unwrap().1, value);

        // Removed: every other entry exactly as it was, `%USERPROFILE%` still
        // unexpanded.
        world.uninstall();
        assert_eq!(path_value(&world).unwrap(), (RegType::REG_EXPAND_SZ, before.to_string()));
    }

    #[test]
    fn a_user_with_no_path_value_gets_an_expandable_one() {
        let world = World::new();
        let _cleanup = Cleanup(world.roots.environment_key.clone());
        world.install("1.0.0", Some("mytool"), &world.options(None)).unwrap();
        let (kind, value) = path_value(&world).unwrap();
        assert_eq!(kind, RegType::REG_EXPAND_SZ);
        assert_eq!(
            value,
            std::path::absolute(world.paths.command_dir()).unwrap().display().to_string()
        );
    }
}
