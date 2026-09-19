//! The `xpack.json` project file.

use std::path::Path;

use serde::{Deserialize, Serialize};
use xpack_core::manifest::{Application, FormatVersion, LaunchSpec, PayloadSpec, UpdateSpec};
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
}

impl ProjectConfig {
    /// Reads `xpack.json`.
    pub(crate) fn load(path: &Path) -> Result<Self> {
        atomic::read_json(path)
    }

    /// Builds the manifest template this configuration describes.
    ///
    /// The payload is left empty on purpose: the packager fills it from the
    /// files it actually reads.
    pub(crate) fn to_manifest(&self, platform: Platform) -> Manifest {
        Manifest {
            format_version: FormatVersion::CURRENT,
            application: self.application.clone(),
            platform: self.platform.unwrap_or(platform),
            launch: self.launch.clone(),
            update: self.update.clone(),
            signing_key: None,
            payload: PayloadSpec::default(),
            created_at: None,
        }
    }
}
