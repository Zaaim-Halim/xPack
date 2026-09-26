//! Shared fixtures: building real signed packages and installations.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use xpack_core::manifest::{Application, FormatVersion, LaunchSpec, PayloadSpec, UpdateSpec};
use xpack_core::{InstallPaths, Manifest, Platform, Version};
use xpack_package::PackageBuilder;
use xpack_security::KeyPair;

/// Builds a real, signed `.xpkg` for `version`.
#[allow(dead_code)]
pub(crate) fn build_package(dir: &Path, key: &KeyPair, version: &str) -> PathBuf {
    build_package_with(dir, key, version, &xpack_core::DesktopSpec::default())
}

/// Builds a package that also asks for a desktop entry.
#[allow(dead_code)]
pub(crate) fn build_package_with(
    dir: &Path,
    key: &KeyPair,
    version: &str,
    desktop: &xpack_core::DesktopSpec,
) -> PathBuf {
    build_package_named(dir, key, version, "Example", desktop)
}

/// Builds a package carrying a chosen display name.
///
/// The name is what executables in an installation are named after, so a test
/// about renaming an application needs to be able to change it.
#[allow(dead_code)]
pub(crate) fn build_package_named(
    dir: &Path,
    key: &KeyPair,
    version: &str,
    display_name: &str,
    desktop: &xpack_core::DesktopSpec,
) -> PathBuf {
    build_package_full(dir, key, version, display_name, desktop, None, &[])
}

/// Builds a package that names the command a terminal starts it by.
#[allow(dead_code)]
pub(crate) fn build_package_commanding(
    dir: &Path,
    key: &KeyPair,
    version: &str,
    command: Option<&str>,
) -> PathBuf {
    build_package_full(
        dir,
        key,
        version,
        "Example",
        &xpack_core::DesktopSpec::default(),
        command,
        &[],
    )
}

/// Builds a package naming a main command and extra ones, each extra running
/// the payload's `bin/app`.
#[allow(dead_code)]
pub(crate) fn build_package_with_commands(
    dir: &Path,
    key: &KeyPair,
    version: &str,
    command: Option<&str>,
    extras: &[&str],
) -> PathBuf {
    build_package_full(
        dir,
        key,
        version,
        "Example",
        &xpack_core::DesktopSpec::default(),
        command,
        extras,
    )
}

fn build_package_full(
    dir: &Path,
    key: &KeyPair,
    version: &str,
    display_name: &str,
    desktop: &xpack_core::DesktopSpec,
    command: Option<&str>,
    extras: &[&str],
) -> PathBuf {
    let payload = dir.join(format!("src-{version}"));
    fs::create_dir_all(payload.join("bin")).unwrap();
    fs::write(payload.join("bin/app"), format!("#!/bin/sh\necho {version}\n")).unwrap();
    fs::write(payload.join("data.txt"), format!("payload for {version}")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(payload.join("bin/app"), fs::Permissions::from_mode(0o755)).unwrap();
    }

    let manifest = Manifest {
        format_version: FormatVersion::CURRENT,
        application: Application {
            id: "com.example.app".into(),
            name: display_name.into(),
            version: Version::parse(version).unwrap(),
            description: None,
            publisher: None,
        },
        platform: Platform::host().unwrap(),
        launch: LaunchSpec {
            executable: "bin/app".into(),
            arguments: vec![],
            working_directory: None,
            keep_working_directory: false,
            environment: BTreeMap::new(),
        },
        update: UpdateSpec::default(),
        health: xpack_core::HealthSpec::default(),
        signing_key: None,
        desktop: desktop.clone(),
        payload: PayloadSpec::default(),
        created_at: None,
        command: command.map(|name| xpack_core::CommandSpec { name: name.into() }),
        commands: extras
            .iter()
            .map(|name| xpack_core::ExtraCommand {
                name: (*name).into(),
                executable: "bin/app".into(),
            })
            .collect(),
    };
    // The format a command needs; what `xpack pack` would declare.
    let manifest = Manifest { format_version: manifest.required_format_version(), ..manifest };

    let out = dir.join(format!("app-{version}.xpkg"));
    PackageBuilder::new(&payload, manifest).build(&out, key).unwrap();
    out
}

/// Writes a stand-in executable, for tests that only need one to exist.
///
/// The desktop entry refuses to point at a launcher that is not on disk, so a
/// test exercising the entry has to supply something for it to find.
#[allow(dead_code)]
pub(crate) fn fake_binary(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
    path
}

/// Directories a desktop entry may be written into, inside a temporary dir.
///
/// Every test that installs a package asking for a shortcut passes these.
/// Without them the entry would land in the real application menu of whoever
/// ran the suite, and stay there.
#[allow(dead_code)]
pub(crate) fn desktop_roots(base: &Path) -> xpack_install::integration::Roots {
    xpack_install::integration::Roots {
        data: base.join("data"),
        home: base.join("home"),
        desktop: Some(base.join("desktop")),
    }
}

/// A temporary installation root plus its layout.
pub(crate) fn install_paths(root: &Path) -> InstallPaths {
    InstallPaths::new(root, "com.example.app").unwrap()
}

/// The names a fresh installation of the fixture gives its executables.
///
/// Derived from the same display name the fixture manifest carries, so a test
/// asserting on a path cannot quietly disagree with what an install writes.
#[allow(dead_code)]
pub(crate) fn installed_names() -> xpack_core::BinaryNames {
    xpack_core::BinaryNames::from_display_name("Example")
}
