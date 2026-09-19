//! The update index: what a server claims is available.

use serde::{Deserialize, Serialize};
use xpack_core::digest::Sha256Digest;
use xpack_core::{Error, Platform, Result, Version};

/// Largest index document accepted, to bound work before parsing.
pub const MAX_INDEX_BYTES: u64 = 1024 * 1024;

/// A package the server says exists.
///
/// Every field here is **attacker-controlled**. `sha256` and `size` allow an
/// early abort on an obviously wrong download; they are never the basis for
/// trusting content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageRef {
    /// Path or URL fragment, relative to the index, naming the package.
    pub file: String,
    /// Claimed size in bytes.
    pub size: u64,
    /// Claimed SHA-256 of the package file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Sha256Digest>,
}

/// What an update server publishes.
///
/// # This document is not a trust input
///
/// The index carries no signature of its own, so a compromised server or CDN
/// can say anything: any version number, any digest, any filename. Treating
/// its digest as authoritative would move the trust root from the publisher's
/// signing key to whoever controls the hosting, which is precisely the
/// attacker this design assumes.
///
/// The index therefore decides only **what to download**. Whether the result
/// may be installed is decided afterwards, by an Ed25519 signature over the
/// package's own manifest, verified against a key pinned in the installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateIndex {
    /// Application the index describes.
    pub application: String,
    /// Version offered.
    pub version: Version,
    /// Platform the offered package targets.
    pub platform: Platform,
    /// Release channel.
    #[serde(default = "default_channel")]
    pub channel: String,
    /// The full package.
    pub package: PackageRef,
    /// Optional release notes URL, for display only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_notes: Option<String>,
}

fn default_channel() -> String {
    "stable".to_string()
}

impl UpdateIndex {
    /// Parses an index document, bounded before allocation.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        if bytes.len() as u64 > MAX_INDEX_BYTES {
            return Err(Error::Transport(format!(
                "update index is {} bytes, limit is {MAX_INDEX_BYTES}",
                bytes.len()
            )));
        }
        serde_json::from_slice(bytes).map_err(|e| Error::json("update index", e))
    }

    /// Rejects an index that does not describe the installation asking for it.
    ///
    /// A server that answers with a different application or platform is
    /// either misconfigured or hostile, and in both cases the answer is the
    /// same: do not download it.
    pub fn ensure_matches(&self, application: &str, host: Platform, channel: &str) -> Result<()> {
        if self.application != application {
            return Err(Error::Transport(format!(
                "update index describes {:?} but this installation is {application:?}",
                self.application
            )));
        }
        if !self.platform.accepts(host) {
            return Err(Error::PlatformMismatch {
                package: self.platform.to_string(),
                host: host.to_string(),
            });
        }
        if self.channel != channel {
            return Err(Error::Transport(format!(
                "update index is for channel {:?} but this installation follows {channel:?}",
                self.channel
            )));
        }
        Ok(())
    }

    /// The local file name to download into.
    ///
    /// Derived here rather than taken from the index. `package.file` is
    /// attacker-controlled and would otherwise become a path under the
    /// installation directory — the same class of hole as an archive entry
    /// name. Ignoring it entirely is simpler than validating it.
    pub fn local_file_name(&self) -> String {
        format!("{}-{}.xpkg.partial", self.application, self.version.to_directory_name())
    }
}
