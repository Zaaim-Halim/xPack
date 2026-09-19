//! Platform identity: the `<os>-<arch>` tuple a package is built for.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Operating systems xPack can target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    /// Microsoft Windows.
    Windows,
    /// Linux (any distribution).
    Linux,
    /// Apple macOS.
    Macos,
}

impl Os {
    /// The operating system this binary was compiled for.
    ///
    /// Returns `None` on targets xPack does not support, so callers fail with
    /// a typed error rather than silently mislabelling a package.
    pub fn host() -> Option<Self> {
        match std::env::consts::OS {
            "windows" => Some(Self::Windows),
            "linux" => Some(Self::Linux),
            "macos" => Some(Self::Macos),
            _ => None,
        }
    }

    /// The canonical lowercase name used in manifests and filenames.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Macos => "macos",
        }
    }

    /// The file extension for executables on this OS, including the dot.
    pub fn executable_suffix(self) -> &'static str {
        match self {
            Self::Windows => ".exe",
            Self::Linux | Self::Macos => "",
        }
    }
}

impl fmt::Display for Os {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Os {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "windows" | "win32" | "win" => Ok(Self::Windows),
            "linux" => Ok(Self::Linux),
            "macos" | "osx" | "darwin" => Ok(Self::Macos),
            other => Err(Error::invalid("os", format!("unknown operating system {other:?}"))),
        }
    }
}

/// CPU architectures xPack can target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    /// 64-bit x86.
    X64,
    /// 64-bit ARM.
    Arm64,
}

impl Arch {
    /// The architecture this binary was compiled for, if supported.
    pub fn host() -> Option<Self> {
        match std::env::consts::ARCH {
            "x86_64" => Some(Self::X64),
            "aarch64" => Some(Self::Arm64),
            _ => None,
        }
    }

    /// The canonical lowercase name used in manifests and filenames.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::X64 => "x64",
            Self::Arm64 => "arm64",
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Arch {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "x64" | "x86_64" | "amd64" => Ok(Self::X64),
            "arm64" | "aarch64" => Ok(Self::Arm64),
            other => Err(Error::invalid("arch", format!("unknown architecture {other:?}"))),
        }
    }
}

/// The `<os>-<arch>` pair a package targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Platform {
    /// Target operating system.
    pub os: Os,
    /// Target CPU architecture.
    #[serde(alias = "architecture")]
    pub arch: Arch,
}

impl Platform {
    /// Creates a platform tuple.
    pub fn new(os: Os, arch: Arch) -> Self {
        Self { os, arch }
    }

    /// The platform this binary is running on.
    pub fn host() -> Result<Self> {
        match (Os::host(), Arch::host()) {
            (Some(os), Some(arch)) => Ok(Self { os, arch }),
            _ => Err(Error::Unsupported(format!(
                "target {}-{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ))),
        }
    }

    /// Returns `true` when a package for `self` may be installed on `host`.
    ///
    /// The rule is strict equality. Emulation layers such as Rosetta 2 and
    /// Windows-on-ARM can *run* a foreign-architecture binary, but a bundled
    /// runtime sized for the wrong architecture is a support nightmare, so
    /// xPack requires an exact match and lets the update server publish a
    /// per-architecture artefact instead.
    pub fn accepts(self, host: Self) -> bool {
        self == host
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.os, self.arch)
    }
}

impl FromStr for Platform {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let (os, arch) = s.split_once('-').ok_or_else(|| {
            Error::invalid("platform", format!("{s:?} is not in <os>-<arch> form"))
        })?;
        Ok(Self { os: os.parse()?, arch: arch.parse()? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_string_form() {
        for text in ["windows-x64", "linux-arm64", "macos-arm64"] {
            let parsed: Platform = text.parse().unwrap();
            assert_eq!(parsed.to_string(), text);
        }
    }

    #[test]
    fn accepts_common_aliases() {
        assert_eq!("darwin-amd64".parse::<Platform>().unwrap().to_string(), "macos-x64");
    }

    #[test]
    fn rejects_cross_architecture_install() {
        let pkg = Platform::new(Os::Macos, Arch::X64);
        let host = Platform::new(Os::Macos, Arch::Arm64);
        assert!(!pkg.accepts(host), "rosetta-style emulation must not be assumed");
    }

    #[test]
    fn host_platform_is_detectable_on_supported_targets() {
        Platform::host().expect("test suite only runs on supported targets");
    }
}
