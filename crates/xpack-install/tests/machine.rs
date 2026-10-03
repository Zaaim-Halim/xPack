//! Installations for every user of the machine, at the library's level.
//!
//! The library does not check that it runs with administrator rights or that
//! the directory is safe; the programs that call it do, before anything is
//! written. So these run as an ordinary user against temporary directories and
//! test what the library decides: what is recorded, what is placed, what is
//! replaced, and where the entries go.

mod common;

use std::path::Path;

use common::{build_package, fake_binary, installed_names};
use xpack_core::state::UpdatePhase;
use xpack_core::{InstallPaths, InstallScope, Version};
use xpack_install::integration::Roots;
use xpack_install::integration::command::CommandRoots;
use xpack_install::{InstallOptions, Installer, LauncherOutcome, TrustDecision, open_and_verify};
use xpack_platform::InstallLock;
use xpack_security::KeyPair;

/// A machine-wide installation named after the application, as one is in
/// `Program Files`, with every entry kept inside `dir`.
struct World {
    dir: tempfile::TempDir,
    key: KeyPair,
    paths: InstallPaths,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = InstallPaths::named(dir.path().join("Example"), "com.example.app").unwrap();
        Self { dir, key: KeyPair::generate().unwrap(), paths }
    }

    fn options(&self, scope: InstallScope) -> InstallOptions {
        let base = self.dir.path();
        InstallOptions {
            activate: true,
            scope,
            launcher: Some(fake_binary(base, "launcher-source")),
            updater: Some(fake_binary(base, "updater-source")),
            uninstaller: Some(fake_binary(base, "uninstaller-source")),
            notifier: Some(fake_binary(base, "notifier-source")),
            desktop_roots: Some(Roots {
                data: base.join("data"),
                home: base.join("home"),
                desktop: None,
                scope,
            }),
            command_roots: Some(CommandRoots {
                bin: base.join("bin"),
                environment_key: "unused".into(),
                scope,
            }),
            ..Default::default()
        }
    }

    fn install(&self, version: &str, options: &InstallOptions) -> xpack_core::Result<()> {
        let package = build_package(self.dir.path(), &self.key, version);
        let lock = InstallLock::acquire(&self.paths).unwrap();
        let mut verified =
            open_and_verify(&package, &lock, &TrustDecision::Explicit(self.key.public()))?;
        Installer::new(&lock).install(&mut verified, options).map(|_| ())
    }

    fn state(&self) -> xpack_core::InstallState {
        xpack_core::store::load(&self.paths.state_file()).unwrap().value
    }
}

fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

#[test]
fn the_scope_is_recorded_and_a_new_version_is_active_at_once() {
    let world = World::new();
    world.install("1.0.0", &world.options(InstallScope::Machine)).unwrap();
    world.install("1.1.0", &world.options(InstallScope::Machine)).unwrap();

    let state = world.state();
    assert_eq!(state.scope, InstallScope::Machine);
    assert_eq!(state.current_version, Some(v("1.1.0")));
    // Nothing that runs as a user could finish a probation here.
    assert_eq!(state.update, UpdatePhase::Idle, "left on probation");
    assert_eq!(state.previous_version, Some(v("1.0.0")), "nothing to roll back to");
}

#[test]
fn the_other_scope_is_refused_and_nothing_changes() {
    let world = World::new();
    world.install("1.0.0", &world.options(InstallScope::User)).unwrap();
    let before = std::fs::read(world.paths.state_file()).unwrap();

    let err = world.install("1.1.0", &world.options(InstallScope::Machine)).unwrap_err();

    assert!(err.to_string().contains("uninstall it first"), "{err}");
    assert_eq!(std::fs::read(world.paths.state_file()).unwrap(), before);
    assert!(!world.paths.version_dir(&v("1.1.0")).exists());
}

#[test]
fn programs_from_an_older_release_are_replaced_in_either_scope_and_the_same_release_never() {
    for scope in [InstallScope::Machine, InstallScope::User] {
        for (recorded, replaced) in [("0.0.1", true), (xpack_core::XPACK_RELEASE, false)] {
            let world = World::new();
            world.install("1.0.0", &world.options(scope)).unwrap();
            let launcher = world.paths.launcher_file_named(&installed_names());
            std::fs::write(&launcher, "an older launcher").unwrap();
            let lock = InstallLock::acquire(&world.paths).unwrap();
            let mut state = lock.load_state().unwrap().value;
            state.runtime_version = Some(v(recorded));
            lock.save_state(&state).unwrap();
            drop(lock);

            world.install("1.1.0", &world.options(scope)).unwrap();

            let now = std::fs::read_to_string(&launcher).unwrap();
            assert_eq!(now != "an older launcher", replaced, "{scope:?}, {recorded}: {now}");
            assert_eq!(
                world.state().runtime_version,
                Some(v(if replaced { xpack_core::XPACK_RELEASE } else { recorded })),
                "{scope:?}, {recorded}"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&launcher).unwrap().permissions().mode();
                assert_eq!(mode & 0o111, 0o111, "{scope:?}: not executable");
            }
            let leftovers: Vec<_> = std::fs::read_dir(world.paths.root())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    name.contains("xpack-new") || name.contains("xpack-old")
                })
                .collect();
            assert!(leftovers.is_empty(), "{scope:?}: {leftovers:?}");
            assert!(!world.paths.runtime_replacement_journal_file().exists());
        }
    }
}

#[test]
fn no_updater_and_no_update_notice_go_in_a_machine_wide_installation() {
    let world = World::new();
    let options = world.options(InstallScope::Machine);
    let package = build_package(world.dir.path(), &world.key, "1.0.0");
    let lock = InstallLock::acquire(&world.paths).unwrap();
    let mut verified =
        open_and_verify(&package, &lock, &TrustDecision::Explicit(world.key.public())).unwrap();
    let installed = Installer::new(&lock).install(&mut verified, &options).unwrap();

    assert_eq!(installed.updater, None);
    assert_eq!(installed.notifier, None);
    assert_eq!(installed.launcher, Some(LauncherOutcome::Installed));
    assert!(!world.paths.updater_file_named(&installed_names()).exists());
}

#[test]
fn an_installation_named_for_people_is_found_again_by_its_own_programs() {
    let world = World::new();
    world.install("1.0.0", &world.options(InstallScope::Machine)).unwrap();

    // What the launcher, the uninstaller and the updater do: they know only
    // the directory they sit in.
    let found = InstallPaths::open(world.paths.root());

    assert_eq!(found.application_id(), Some("com.example.app"));
    assert!(
        !found.logs_dir().starts_with(world.paths.root()),
        "a user's logs would go into an installation they cannot write to"
    );
    assert!(found.per_user_dir().ends_with(Path::new("xpack-user/com.example.app")));
}

#[cfg(unix)]
#[test]
fn a_command_for_everyone_is_not_put_where_a_user_could_change_it() {
    // The attack: `/usr/local/bin` belonging to one user, who could then
    // replace what every other user runs. Here the directory is a temporary
    // one, which belongs to the user running the suite.
    let world = World::new();
    let package =
        common::build_package_commanding(world.dir.path(), &world.key, "1.0.0", Some("example"));
    let lock = InstallLock::acquire(&world.paths).unwrap();
    let mut verified =
        open_and_verify(&package, &lock, &TrustDecision::Explicit(world.key.public())).unwrap();
    let options = world.options(InstallScope::Machine);
    let installed = Installer::new(&lock).install(&mut verified, &options).unwrap();

    let xpack_install::DesktopOutcome::Failed(reason) = &installed.command else {
        panic!("the command was put in place: {:?}", installed.command);
    };
    assert!(reason.contains("does not belong to root"), "{reason}");
    assert!(!world.dir.path().join("bin").join("example").exists());
}

#[cfg(unix)]
#[test]
fn nothing_in_an_installation_for_everyone_is_left_writable_by_others() {
    // The attack, whatever carries it: an installation directory that came
    // into being writable by everyone. On a Linux machine whose /opt has a
    // default access list, what is created there is, whatever the umask.
    use std::os::unix::fs::PermissionsExt;
    let world = World::new();
    std::fs::create_dir_all(world.paths.root().join("versions")).unwrap();
    for dir in [world.paths.root(), &world.paths.root().join("versions")] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    }

    // Two versions, so the second's activation writes state after the first
    // install: nothing written last may escape either.
    world.install("1.0.0", &world.options(InstallScope::Machine)).unwrap();
    std::fs::set_permissions(world.paths.root(), std::fs::Permissions::from_mode(0o777)).unwrap();
    world.install("1.1.0", &world.options(InstallScope::Machine)).unwrap();

    let mut pending = vec![world.paths.root().to_path_buf()];
    while let Some(path) = pending.pop() {
        let meta = std::fs::symlink_metadata(&path).unwrap();
        if meta.file_type().is_symlink() {
            continue;
        }
        let mode = meta.permissions().mode();
        assert_eq!(mode & 0o022, 0, "{} is writable by others: {mode:o}", path.display());
        if meta.is_dir() {
            pending.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
        }
    }
}
