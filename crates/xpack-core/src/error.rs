//! The single error type shared across xPack crates.

use std::path::PathBuf;

/// Convenience alias for fallible xPack operations.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Every failure mode xPack can surface to a caller.
///
/// Variants are deliberately coarse but *typed*: callers such as the updater
/// branch on them to decide between retry, rollback and abort, so adding a
/// catch-all string variant would defeat the point.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An underlying filesystem or process I/O call failed.
    #[error("i/o error at {path}: {source}")]
    Io {
        /// Path the failing operation was acting on.
        path: PathBuf,
        /// Underlying operating-system error.
        #[source]
        source: std::io::Error,
    },

    /// An I/O call that is not attributable to a single path.
    #[error("i/o error: {0}")]
    BareIo(#[from] std::io::Error),

    /// A JSON document could not be parsed or serialised.
    #[error("malformed json in {context}: {source}")]
    Json {
        /// Human-readable description of the document being handled.
        context: String,
        /// Underlying serde failure.
        #[source]
        source: serde_json::Error,
    },

    /// A document parsed correctly but violates a domain rule.
    #[error("invalid {subject}: {reason}")]
    Invalid {
        /// What was being validated, e.g. `"manifest"`.
        subject: String,
        /// Why it was rejected.
        reason: String,
    },

    /// The package declares a format version this build cannot interpret.
    ///
    /// xPack fails closed here: a newer package may rely on integrity rules
    /// this binary does not implement, so trusting it would be unsound.
    #[error(
        "package format version {found} is newer than the maximum supported version {supported}; \
         upgrade xPack to install this package"
    )]
    UnsupportedFormatVersion {
        /// Version declared by the package.
        found: u32,
        /// Highest version this build understands.
        supported: u32,
    },

    /// A cryptographic check failed. Never downgrade this to a warning.
    #[error("integrity failure: {0}")]
    Integrity(String),

    /// A package entry path was unsafe to extract (traversal, absolute, …).
    #[error("unsafe package entry {entry:?}: {reason}")]
    UnsafeEntry {
        /// The raw entry name as stored in the archive.
        entry: String,
        /// Why the entry was rejected.
        reason: String,
    },

    /// The requested version is not installed.
    ///
    /// Carries **only the version**, because the message wraps it: passing a
    /// whole sentence produces `version <sentence> is not installed`. Use
    /// [`Error::invalid`] for anything that needs to explain itself.
    #[error("version {0} is not installed")]
    VersionNotInstalled(String),

    /// The operation would move the installation to an older version.
    #[error("refusing to move from {current} to {candidate}: downgrades are rejected by default")]
    DowngradeRejected {
        /// Currently active version.
        current: String,
        /// Version that was offered.
        candidate: String,
    },

    /// The package targets a platform other than the running one.
    #[error("package targets {package} but this machine is {host}")]
    PlatformMismatch {
        /// Platform declared by the package.
        package: String,
        /// Platform detected at runtime.
        host: String,
    },

    /// Another xPack process holds the installation lock.
    #[error("another xpack operation is already running for this installation ({0})")]
    Locked(String),

    /// The operation is not implemented for the current platform.
    #[error("{0} is not supported on this platform yet")]
    Unsupported(String),

    /// An update could not be retrieved from the update server.
    #[error("update transport error: {0}")]
    Transport(String),

    /// A launched process failed or was rejected.
    #[error("launch failed: {0}")]
    Launch(String),

    /// The disk does not have room for what is about to be written.
    ///
    /// Checked before writing, so the user hears it while nothing has
    /// changed, rather than from a write that fails part-way through and a
    /// disk left full for every other program.
    #[error(
        "not enough free space for {what} at {}: needs {}, only {} free",
        path.display(),
        crate::progress::format_bytes(*needed),
        crate::progress::format_bytes(*available)
    )]
    NotEnoughSpace {
        /// What was about to be written, such as "this version".
        what: String,
        /// Where it would have been written.
        path: PathBuf,
        /// Bytes required, including the margin kept free.
        needed: u64,
        /// Bytes the volume has free for this user.
        available: u64,
    },
}

impl Error {
    /// Builds an [`Error::Io`] that remembers which path failed.
    ///
    /// Bare `io::Error` values lose the path, which is the first thing anyone
    /// debugging a failed install wants to know.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io { path: path.into(), source }
    }

    /// Builds an [`Error::Json`] tagged with the document being handled.
    pub fn json(context: impl Into<String>, source: serde_json::Error) -> Self {
        Self::Json { context: context.into(), source }
    }

    /// Builds an [`Error::Invalid`] for a domain-rule violation.
    pub fn invalid(subject: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Invalid { subject: subject.into(), reason: reason.into() }
    }

    /// Returns `true` when the failure means a package must not be trusted.
    ///
    /// The updater uses this to decide whether a downloaded artefact should be
    /// purged rather than retried.
    pub fn is_integrity_failure(&self) -> bool {
        matches!(
            self,
            Self::Integrity(_)
                | Self::UnsafeEntry { .. }
                | Self::UnsupportedFormatVersion { .. }
                | Self::DowngradeRejected { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_enough_space_says_how_much_in_words_a_person_reads() {
        let error = Error::NotEnoughSpace {
            what: "this version".into(),
            path: PathBuf::from("/apps"),
            needed: 300 * 1024 * 1024,
            available: 120 * 1024 * 1024,
        };
        assert_eq!(
            error.to_string(),
            "not enough free space for this version at /apps: needs 300.0 MiB, only 120.0 MiB free"
        );
    }
}
