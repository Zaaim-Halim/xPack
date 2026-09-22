//! Whether an installation may look for updates on its own.
//!
//! # Why this exists
//!
//! Everything else about updating is the publisher's to decide, and belongs in
//! the signed manifest where a client cannot be lied to about it. This one
//! question is not theirs: the machine belongs to whoever is running the
//! application, and on some machines nothing may reach the network without
//! someone approving it first — an air-gapped network, a validated
//! environment, a laptop on a metered connection abroad.
//!
//! Without an answer here, the only way to stop an installation checking is to
//! delete the updater binary, which the next install puts back. Every
//! comparable tool has a switch: Sparkle has `SUEnableAutomaticChecks`, Chrome
//! has administrative policy, Homebrew has `HOMEBREW_NO_AUTO_UPDATE`.
//!
//! # What "off" stops, and what it does not
//!
//! It stops xPack deciding to check by itself: the check the launcher makes at
//! startup, and the checking it does while an application runs.
//!
//! It does not stop `xpack update`. Somebody typing that has asked for an
//! update in the clearest terms available, and a switch that ignored them
//! would be a switch people work around rather than use. The same reasoning
//! Homebrew applies to `HOMEBREW_NO_AUTO_UPDATE`, which suppresses the
//! automatic check and leaves `brew upgrade` alone.
//!
//! It does not reach back into a version already downloaded and verified. One
//! that is staged when the switch is thrown is still activated at the next
//! start, because refusing to would leave an installation holding a version it
//! will neither run nor forget. Turning updates off is about what happens
//! next, not about undoing what already happened — and anything staged is
//! reported when the switch is thrown, rather than left to be discovered.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::paths::InstallPaths;
use crate::store;

/// Highest [`UpdatePolicy`] document this build understands.
const POLICY_FORMAT_VERSION: u32 = 1;

/// The environment variable that turns automatic checks off everywhere.
///
/// For the cases a per-installation file cannot serve: a build server, a
/// container, a fleet where the answer is the same for every application and
/// nobody wants to write a file into each one.
pub const NO_UPDATE_ENV: &str = "XPACK_NO_UPDATE";

/// What an installation has been told about checking for updates on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdatePolicy {
    /// Format of this document, so a newer one is refused rather than misread.
    pub format_version: u32,

    /// Whether xPack may check for updates without being asked.
    ///
    /// On unless somebody turned it off. An installation with no policy file
    /// at all -- which is every installation until someone decides otherwise
    /// -- behaves exactly as it did before this existed.
    pub automatic: bool,
}

impl Default for UpdatePolicy {
    fn default() -> Self {
        Self { format_version: POLICY_FORMAT_VERSION, automatic: true }
    }
}

impl UpdatePolicy {
    /// Reads the policy, falling back to the default when there is none.
    ///
    /// A missing file is the ordinary case and means "not decided", which is
    /// the same as on. A file that cannot be read is treated the same way, and
    /// deliberately: refusing to update because a preferences file is corrupt
    /// would turn a cosmetic fault into a security one, since the updates it
    /// stopped include the ones that fix things.
    pub fn load(paths: &InstallPaths) -> Self {
        store::load::<Self>(&paths.update_policy_file())
            .ok()
            .map(|loaded| loaded.value)
            .filter(|policy| policy.format_version <= POLICY_FORMAT_VERSION)
            .unwrap_or_default()
    }

    /// Writes the policy, creating the configuration directory if needed.
    pub fn save(&self, paths: &InstallPaths) -> Result<()> {
        crate::atomic::create_dir_all(&paths.config_dir())?;
        store::save(&paths.update_policy_file(), self)
    }

    /// The policy that answers `automatic`.
    pub fn automatic(automatic: bool) -> Self {
        Self { automatic, ..Self::default() }
    }
}

/// Why an installation is not checking for updates on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomaticChecks {
    /// Nothing is stopping them.
    Allowed,
    /// This installation was told not to.
    StoppedByPolicy,
    /// The environment says so, for every installation this process can see.
    StoppedByEnvironment,
}

impl AutomaticChecks {
    /// Whether a check may be made without being asked for.
    pub fn allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }

    /// A phrase naming the reason, for a log line or a message to a user.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::StoppedByPolicy => "turned off for this installation",
            Self::StoppedByEnvironment => "turned off by XPACK_NO_UPDATE",
        }
    }
}

/// Decides whether this installation may check for updates by itself.
pub fn automatic_checks(paths: &InstallPaths) -> AutomaticChecks {
    AutomaticChecks::decide(&UpdatePolicy::load(paths), std::env::var_os(NO_UPDATE_ENV).as_deref())
}

impl AutomaticChecks {
    /// The decision itself, given a policy and what the environment said.
    ///
    /// Separate from reading either, so it can be exercised without setting a
    /// variable in the process running the test -- which in edition 2024 is an
    /// unsafe operation, and in any edition is a race against every other test
    /// sharing the process.
    ///
    /// The environment is asked first: it is how the owner of a machine says
    /// something about every installation on it at once, and a file inside one
    /// of them cannot overrule the machine it sits on.
    pub fn decide(policy: &UpdatePolicy, environment: Option<&std::ffi::OsStr>) -> Self {
        if environment_forbids_updates(environment) {
            return Self::StoppedByEnvironment;
        }
        if policy.automatic { Self::Allowed } else { Self::StoppedByPolicy }
    }
}

/// Whether an environment variable's value asks for updates to stop.
///
/// Set to anything is a yes, because that is how these variables are used and
/// what somebody typing `XPACK_NO_UPDATE=1` means. The exceptions are the
/// values that plainly say otherwise: a variable set to `0` or `false` reads
/// as "no, do not stop", and honouring the name over the value would surprise
/// the one person careful enough to write it.
fn environment_forbids_updates(value: Option<&std::ffi::OsStr>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let Some(text) = value.to_str() else {
        // Not text this build can read, but it was set to something.
        return true;
    };
    !matches!(text.trim().to_ascii_lowercase().as_str(), "" | "0" | "false" | "no" | "off")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    fn installation() -> (tempfile::TempDir, InstallPaths) {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let paths = InstallPaths::from_application_dir(dir.path());
        (dir, paths)
    }

    #[test]
    fn an_installation_that_was_never_asked_checks_for_updates() {
        // Every installation until somebody decides otherwise. The absence of
        // a preference is not a preference for silence.
        let (_dir, paths) = installation();
        assert!(UpdatePolicy::load(&paths).automatic);
        assert_eq!(automatic_checks(&paths), AutomaticChecks::Allowed);
    }

    #[test]
    fn turning_it_off_is_remembered() {
        let (_dir, paths) = installation();
        UpdatePolicy::automatic(false).save(&paths).expect("the policy to be written");

        assert!(!UpdatePolicy::load(&paths).automatic);
        assert_eq!(automatic_checks(&paths), AutomaticChecks::StoppedByPolicy);
    }

    #[test]
    fn turning_it_back_on_is_remembered_too() {
        let (_dir, paths) = installation();
        UpdatePolicy::automatic(false).save(&paths).expect("off");
        UpdatePolicy::automatic(true).save(&paths).expect("on again");

        assert_eq!(automatic_checks(&paths), AutomaticChecks::Allowed);
    }

    #[test]
    fn a_policy_file_that_cannot_be_read_does_not_stop_updates() {
        // Refusing to update because a preferences file is corrupt would turn
        // a cosmetic fault into a security one: the updates it stopped include
        // the ones that fix things.
        let (_dir, paths) = installation();
        crate::atomic::create_dir_all(&paths.config_dir()).expect("the directory");
        std::fs::write(paths.update_policy_file(), b"{ not json").expect("a corrupt file");

        assert!(UpdatePolicy::load(&paths).automatic);
    }

    #[test]
    fn a_policy_from_a_newer_xpack_does_not_stop_updates_either() {
        // Same reasoning. A document this build cannot interpret says nothing
        // about what its author wanted, and guessing "off" fails closed in the
        // direction that leaves known holes unpatched.
        let (_dir, paths) = installation();
        let newer = UpdatePolicy { format_version: POLICY_FORMAT_VERSION + 1, automatic: false };
        newer.save(&paths).expect("the policy to be written");

        assert!(UpdatePolicy::load(&paths).automatic);
    }

    #[test]
    fn the_environment_says_so_for_every_installation_at_once() {
        for value in ["1", "true", "yes", "anything at all"] {
            assert!(
                environment_forbids_updates(Some(OsStr::new(value))),
                "{value:?} should stop automatic checks"
            );
        }
    }

    #[test]
    fn a_variable_that_plainly_says_no_is_believed() {
        // Somebody careful enough to write `XPACK_NO_UPDATE=0` meant it.
        for value in ["0", "false", "no", "off", "", "  FALSE  "] {
            assert!(
                !environment_forbids_updates(Some(OsStr::new(value))),
                "{value:?} should not stop automatic checks"
            );
        }
        assert!(!environment_forbids_updates(None));
    }

    #[test]
    fn the_environment_overrules_the_file() {
        // A machine's owner says something about every installation on it, and
        // a file inside one of them cannot overrule the machine it sits on.
        let allowed = UpdatePolicy::automatic(true);
        assert_eq!(
            AutomaticChecks::decide(&allowed, Some(OsStr::new("1"))),
            AutomaticChecks::StoppedByEnvironment
        );
    }

    #[test]
    fn a_quiet_environment_leaves_the_decision_to_the_file() {
        assert_eq!(
            AutomaticChecks::decide(&UpdatePolicy::automatic(true), None),
            AutomaticChecks::Allowed
        );
        assert_eq!(
            AutomaticChecks::decide(&UpdatePolicy::automatic(false), None),
            AutomaticChecks::StoppedByPolicy
        );
    }

    #[test]
    fn every_reason_says_why_in_words() {
        assert!(AutomaticChecks::Allowed.allowed());
        assert!(!AutomaticChecks::StoppedByPolicy.allowed());
        assert!(!AutomaticChecks::StoppedByEnvironment.allowed());
        assert!(AutomaticChecks::StoppedByEnvironment.describe().contains("XPACK_NO_UPDATE"));
    }

    #[test]
    fn the_policy_lives_beside_the_trust_store_and_survives_updates() {
        // Under config/, which no update writes to -- unlike state/, which
        // xPack rewrites on every operation.
        let (_dir, paths) = installation();
        assert_eq!(paths.update_policy_file().parent(), Some(paths.config_dir().as_path()));
        assert_ne!(paths.update_policy_file(), paths.trust_file());
    }
}
