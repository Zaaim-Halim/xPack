//! Shared fixtures: building real signed packages and installations.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use xpack_core::manifest::{Application, FormatVersion, LaunchSpec, PayloadSpec, UpdateSpec};
use xpack_core::{InstallPaths, Manifest, Platform, Version};
use xpack_package::PackageBuilder;
use xpack_security::KeyPair;

/// Builds a real, signed `.xpkg` for `version`.
pub(crate) fn build_package(dir: &Path, key: &KeyPair, version: &str) -> PathBuf {
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
            name: "Example".into(),
            version: Version::parse(version).unwrap(),
            description: None,
            publisher: None,
        },
        platform: Platform::host().unwrap(),
        launch: LaunchSpec {
            executable: "bin/app".into(),
            arguments: vec![],
            working_directory: None,
            environment: BTreeMap::new(),
        },
        update: UpdateSpec::default(),
        health: xpack_core::HealthSpec::default(),
        signing_key: None,
        payload: PayloadSpec::default(),
        created_at: None,
    };

    let out = dir.join(format!("app-{version}.xpkg"));
    PackageBuilder::new(&payload, manifest).build(&out, key).unwrap();
    out
}

/// A temporary installation root plus its layout.
pub(crate) fn install_paths(root: &Path) -> InstallPaths {
    InstallPaths::new(root, "com.example.app").unwrap()
}
