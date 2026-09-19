//! Semantic version handling.
//!
//! Ordering is delegated wholesale to the `semver` crate. Hand-rolled
//! pre-release comparison is a classic source of anti-downgrade bypasses
//! (`1.2.0-rc.2` vs `1.2.0-rc.10`), so xPack never implements its own.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A `SemVer` 2.0.0 application version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Version(semver::Version);

impl Version {
    /// Parses a `SemVer` string such as `1.2.0` or `1.2.0-rc.1`.
    pub fn parse(text: &str) -> Result<Self> {
        semver::Version::parse(text)
            .map(Self)
            .map_err(|e| Error::invalid("version", format!("{text:?}: {e}")))
    }

    /// Returns `true` when this version carries a pre-release tag.
    pub fn is_prerelease(&self) -> bool {
        !self.0.pre.is_empty()
    }

    /// Borrows the underlying `semver::Version`.
    pub fn as_semver(&self) -> &semver::Version {
        &self.0
    }

    /// Returns a filesystem-safe rendering of the version.
    ///
    /// `SemVer`'s grammar (`0-9A-Za-z.-+`) is already safe on every supported
    /// filesystem except for `+` build metadata, which Windows tolerates but
    /// which confuses URL handling, so it is mapped to `_`.
    pub fn to_directory_name(&self) -> String {
        self.to_string().replace('+', "_")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromStr for Version {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

impl From<semver::Version> for Version {
    fn from(v: semver::Version) -> Self {
        Self(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_prereleases_numerically_not_lexically() {
        // The bug this guards: string comparison puts "rc.10" before "rc.2".
        let rc2 = Version::parse("1.2.0-rc.2").unwrap();
        let rc10 = Version::parse("1.2.0-rc.10").unwrap();
        assert!(rc2 < rc10, "rc.2 must sort before rc.10");
    }

    #[test]
    fn prerelease_sorts_below_its_release() {
        assert!(Version::parse("1.2.0-rc.1").unwrap() < Version::parse("1.2.0").unwrap());
    }

    #[test]
    fn build_metadata_is_ignored_for_ordering() {
        // Build metadata is excluded from precedence comparison.
        let a = Version::parse("1.0.0+build.1").unwrap();
        let b = Version::parse("1.0.0+build.2").unwrap();
        assert_eq!(a.as_semver().cmp_precedence(b.as_semver()), std::cmp::Ordering::Equal);
    }

    #[test]
    fn build_metadata_is_stripped_from_directory_names() {
        assert_eq!(Version::parse("1.0.0+b.1").unwrap().to_directory_name(), "1.0.0_b.1");
    }

    #[test]
    fn rejects_non_semver() {
        assert!(Version::parse("1.0").is_err());
        assert!(Version::parse("").is_err());
    }
}
