//! The `xpack.json` project file.

use std::path::Path;

use serde::{Deserialize, Serialize};
use xpack_core::manifest::{
    Application, CommandSpec, DesktopSpec, ExtraCommand, FormatVersion, HealthSpec, InstanceSpec,
    LaunchSpec, PayloadSpec, UpdateSpec,
};
use xpack_core::{Manifest, Platform, Result, atomic};

/// A project's packaging configuration.
///
/// Everything the manifest needs except the payload inventory, which is always
/// computed from the files on disk. A configuration file that could declare
/// its own hashes would let a typo produce a package that fails on every
/// client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProjectConfig {
    /// Application identity.
    pub(crate) application: Application,
    /// Target platform. Defaults to the host when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) platform: Option<Platform>,
    /// How the application is started.
    pub(crate) launch: LaunchSpec,
    /// Update configuration.
    #[serde(default)]
    pub(crate) update: UpdateSpec,
    /// How a newly activated version proves it started.
    #[serde(default)]
    pub(crate) health: HealthSpec,
    /// How the application appears in the user's desktop environment.
    #[serde(default)]
    pub(crate) desktop: DesktopSpec,
    /// The command a terminal starts the application by, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) command: Option<CommandSpec>,
    /// Further commands, each starting another program the package ships.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) commands: Vec<ExtraCommand>,
    /// How many copies of the application may run at once.
    #[serde(default, skip_serializing_if = "InstanceSpec::is_default")]
    pub(crate) instance: InstanceSpec,
    /// Whether the installer, and the packages, are locked with a password.
    #[serde(default, skip_serializing_if = "Protection::is_off")]
    pub(crate) protection: Protection,
}

/// Locking the application behind a password.
///
/// Both off unless turned on. The password itself is never here: it is read
/// from the environment variable [`Protection::password_env`] names, or from
/// standard input, when a command needs it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Protection {
    /// The installer asks for the password before it installs anything.
    #[serde(default)]
    pub(crate) installer: bool,
    /// Packages, updates and deltas are sealed with the password too, so a
    /// package taken from the update server cannot be installed without it.
    /// Needs `installer`: an installation gets the key that opens its updates
    /// from the password its installer was given.
    #[serde(default)]
    pub(crate) packages: bool,
    /// The environment variable holding the password. `XPACK_PASSWORD` when
    /// left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) password_env: Option<String>,
}

impl Protection {
    fn is_off(&self) -> bool {
        self == &Self::default()
    }

    /// Refuses the one combination that cannot protect anything.
    pub(crate) fn validate(&self) -> Result<()> {
        if self.packages && !self.installer {
            return Err(xpack_core::Error::invalid(
                "protection",
                "sealed packages need the installer lock: an installation gets the key that \
                 opens its updates from the password its installer is given",
            ));
        }
        Ok(())
    }
}

impl ProjectConfig {
    /// Reads `xpack.json`.
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let config: Self = atomic::read_json(path)?;
        config.protection.validate()?;
        Ok(config)
    }

    /// Builds the manifest template this configuration describes.
    ///
    /// The payload is left empty on purpose: the packager fills it from the
    /// files it actually reads.
    ///
    /// It declares the lowest format that can express it, so a package that
    /// uses nothing new stays installable by every release already out there.
    pub(crate) fn to_manifest(&self, platform: Platform) -> Manifest {
        let mut manifest = Manifest {
            format_version: FormatVersion::CURRENT,
            application: self.application.clone(),
            platform: self.platform.unwrap_or(platform),
            launch: self.launch.clone(),
            update: self.update.clone(),
            health: self.health.clone(),
            desktop: self.desktop.clone(),
            signing_key: None,
            payload: PayloadSpec::default(),
            created_at: None,
            command: self.command.clone(),
            commands: self.commands.clone(),
            instance: self.instance.clone(),
        };
        manifest.format_version = manifest.required_format_version();
        manifest
    }
}
