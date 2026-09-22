//! The line under the location: what is there, and what that allows.

use xpack_install::Existing;

use super::text::{Key, Texts};
use crate::engine::{Inspection, RootProblem};

/// How serious a status line is, which decides its icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Informational.
    Info,
    /// Something stops the install, but nothing is wrong with the choice.
    Warning,
    /// The choice itself is wrong.
    Error,
}

/// What an allowed installation will be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    /// A first installation, or one with no active version.
    New,
    /// An upgrade from this version.
    Upgrade(xpack_core::Version),
    /// Restoring the files of the version being installed.
    Repair,
}

impl InstallKind {
    /// The kind an inspection allows, or `None` when it allows nothing.
    pub fn of(existing: &Existing) -> Option<Self> {
        match existing {
            Existing::Nothing | Existing::Inactive => Some(Self::New),
            Existing::Older(version) => Some(Self::Upgrade(version.clone())),
            Existing::Damaged => Some(Self::Repair),
            _ => None,
        }
    }
}

/// The status line, and what it allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// Which icon to show.
    pub severity: Severity,
    /// A bold first sentence, where the state has one.
    pub heading: Option<String>,
    /// The explanation.
    pub body: String,
    /// Whether installing may go ahead.
    pub allows_install: bool,
    /// Whether a Retry button belongs beside it.
    pub offers_retry: bool,
    /// Whether to offer starting what is already installed.
    pub offers_launch: bool,
}

impl Status {
    /// The status for an inspection, or for one not finished yet.
    ///
    /// Whether installing is allowed is [`Existing::allows_install`], the
    /// installer's own answer. Nothing here second-guesses it.
    pub fn of(inspection: Option<&Inspection>, texts: &Texts) -> Self {
        let plain = |severity, body: String| Self {
            severity,
            heading: None,
            body,
            allows_install: false,
            offers_retry: false,
            offers_launch: false,
        };

        let Some(inspection) = inspection else {
            return plain(Severity::Info, texts.line(Key::StatusChecking));
        };
        let existing = match &inspection.verdict {
            Ok(existing) => existing,
            Err(problem) => {
                let key = match problem {
                    RootProblem::NotAbsolute => Key::StatusNotAbsolute,
                    RootProblem::NotWritable => Key::StatusNotWritable,
                    RootProblem::TooDeep => Key::StatusTooDeep,
                };
                return plain(Severity::Error, texts.line(key));
            }
        };

        let allowed = |severity, body| Self {
            allows_install: existing.allows_install(),
            ..plain(severity, body)
        };
        match existing {
            Existing::Nothing | Existing::Inactive => {
                allowed(Severity::Info, texts.line(Key::StatusNew))
            }
            Existing::Older(old) => allowed(
                Severity::Info,
                texts.line_with(Key::StatusUpgrade, &[("old", &old.to_string())]),
            ),
            Existing::Damaged => allowed(Severity::Warning, texts.line(Key::StatusDamaged)),
            Existing::Installed => Self {
                offers_launch: true,
                ..plain(Severity::Info, texts.line(Key::StatusInstalled))
            },
            Existing::Newer(newer) => Self {
                heading: Some(
                    texts.line_with(Key::StatusNewerHeading, &[("v", &newer.to_string())]),
                ),
                ..plain(Severity::Warning, texts.line(Key::StatusNewerBody))
            },
            Existing::Busy => Self {
                heading: Some(texts.line(Key::StatusBusyHeading)),
                offers_retry: true,
                ..plain(Severity::Warning, texts.line(Key::StatusBusyBody))
            },
            Existing::Unreadable(message) => plain(Severity::Error, message.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use xpack_core::Version;

    use super::*;
    use crate::model::text::{Facts, Flavour};

    fn texts() -> Texts {
        let facts = Facts {
            name: "App".into(),
            version: Version::parse("2.0.0").unwrap(),
            publisher: None,
            description: None,
        };
        Texts::new(Flavour::Windows, facts, BTreeMap::new())
    }

    fn status(verdict: Result<Existing, RootProblem>) -> Status {
        let inspection = Inspection { target: PathBuf::from("/r/app"), verdict };
        Status::of(Some(&inspection), &texts())
    }

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn the_status_allows_exactly_what_the_installer_allows() {
        let every = [
            Existing::Nothing,
            Existing::Inactive,
            Existing::Older(v("1.0.0")),
            Existing::Installed,
            Existing::Damaged,
            Existing::Newer(v("3.0.0")),
            Existing::Busy,
            Existing::Unreadable("x".into()),
        ];
        for existing in every {
            let allowed = existing.allows_install();
            assert_eq!(status(Ok(existing.clone())).allows_install, allowed, "{existing:?}");
            assert_eq!(InstallKind::of(&existing).is_some(), allowed, "{existing:?}");
        }
    }

    #[test]
    fn nothing_there_installs() {
        assert_eq!(status(Ok(Existing::Nothing)).body, "App 2.0.0 will be installed here.");
    }

    #[test]
    fn an_older_version_upgrades() {
        assert_eq!(status(Ok(Existing::Older(v("1.4.2")))).body, "Will upgrade App 1.4.2 → 2.0.0.");
    }

    #[test]
    fn the_same_version_installed_is_not_reinstalled_but_can_be_opened() {
        let status = status(Ok(Existing::Installed));
        assert!(!status.allows_install);
        assert!(status.offers_launch);
    }

    #[test]
    fn a_newer_version_blocks_and_waiting_will_not_help() {
        let status = status(Ok(Existing::Newer(v("3.0.0"))));
        assert_eq!(status.severity, Severity::Warning);
        assert_eq!(
            status.heading.as_deref(),
            Some("A newer version (3.0.0) is already installed here.")
        );
        assert!(!status.offers_retry);
    }

    #[test]
    fn a_busy_installation_blocks_and_offers_a_retry() {
        let status = status(Ok(Existing::Busy));
        assert!(!status.allows_install);
        assert!(status.offers_retry);
    }

    #[test]
    fn a_bad_root_is_an_error() {
        for problem in [RootProblem::NotAbsolute, RootProblem::NotWritable, RootProblem::TooDeep] {
            let status = status(Err(problem));
            assert!(!status.allows_install, "{problem:?}");
            assert_eq!(status.severity, Severity::Error, "{problem:?}");
        }
    }

    #[test]
    fn an_unreadable_installation_shows_the_engine_message() {
        assert_eq!(
            status(Ok(Existing::Unreadable("state is corrupt".into()))).body,
            "state is corrupt"
        );
    }

    #[test]
    fn an_inspection_not_finished_yet_blocks() {
        let status = Status::of(None, &texts());
        assert!(!status.allows_install);
        assert_eq!(status.body, "Checking this folder…");
    }
}
