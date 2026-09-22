//! Every word the wizard shows.
//!
//! Front-ends never write page copy of their own. They ask for a [`Key`] and
//! draw what comes back, which is what lets a publisher reword a line, keeps
//! the two platforms saying the same thing, and leaves room to translate it
//! all later without touching drawing code.
//!
//! # Substitution happens once
//!
//! Placeholders such as `{name}` are replaced in a single pass, and a value
//! that was substituted in is never scanned again. A publisher calling
//! themselves `{version}` gets their name printed, not a version number. Every
//! value here comes from the package or the filesystem, and none of it is
//! allowed to change what the surrounding sentence says.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use xpack_core::{Error, Result, Version};

/// Which platform's conventions the wording follows.
///
/// A parameter rather than a compile-time choice, so both are tested on every
/// machine. Windows wizards say "Next >" and "Setup"; macOS says "Continue"
/// and never "Setup".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    /// Wizard97 conventions.
    Windows,
    /// macOS installer conventions.
    Mac,
}

impl Flavour {
    /// The conventions of the platform this was built for.
    pub fn native() -> Self {
        if cfg!(windows) { Self::Windows } else { Self::Mac }
    }
}

/// The facts about what is being installed.
///
/// Taken from the verified package manifest and from nowhere else, because
/// they are what a person uses to decide whether to trust the installer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// The application's display name.
    pub name: String,
    /// The version being installed.
    pub version: Version,
    /// Who publishes it, when the manifest says.
    pub publisher: Option<String>,
    /// A one-line description, when the manifest has one.
    pub description: Option<String>,
}

/// The lines a publisher may reword.
///
/// Deliberately few. Headings, buttons and warnings stay as written, so a
/// wizard cannot be made to mislabel what a button does or play down a
/// warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextOverride {
    /// The paragraph on the welcome page.
    Welcome,
    /// The paragraph on a successful finish page.
    Finish,
}

impl TextOverride {
    fn key(self) -> Key {
        match self {
            Self::Welcome => Key::Welcome,
            Self::Finish => Key::Finish,
        }
    }
}

/// The longest replacement accepted, in characters. Beyond this a paragraph
/// no longer fits a fixed-size wizard page.
pub const OVERRIDE_MAX_CHARS: usize = 400;

/// Placeholders a replacement may use. Only facts that always exist, so a
/// replacement can never render with a hole in it.
const OVERRIDE_PLACEHOLDERS: [&str; 2] = ["name", "version"];

/// Checks a publisher's replacement line.
pub(crate) fn validate_override(key: TextOverride, value: &str) -> Result<()> {
    let subject = format!("installer text {:?}", key.key());
    if value.trim().is_empty() {
        return Err(Error::invalid(subject, "must not be empty"));
    }
    let length = value.chars().count();
    if length > OVERRIDE_MAX_CHARS {
        return Err(Error::invalid(
            subject,
            format!("is {length} characters; the limit is {OVERRIDE_MAX_CHARS}"),
        ));
    }
    if value.chars().any(|c| c.is_control() && c != '\n') {
        return Err(Error::invalid(subject, "contains a control character"));
    }
    for placeholder in placeholders(value) {
        if !OVERRIDE_PLACEHOLDERS.contains(&placeholder) {
            return Err(Error::invalid(
                subject,
                format!(
                    "uses {{{placeholder}}}; only {} are available",
                    OVERRIDE_PLACEHOLDERS.map(|p| format!("{{{p}}}")).join(" and ")
                ),
            ));
        }
    }
    Ok(())
}

/// A line of wizard text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(missing_docs)] // Each variant is documented by its default text below.
pub enum Key {
    WindowTitle,
    Back,
    Next,
    Install,
    Close,
    Cancel,
    Browse,
    Retry,
    Launch,

    WelcomeTitle,
    WelcomeHeadline,
    WelcomeByline,
    WelcomeVerified,
    Welcome,
    WelcomeHint,

    LicenceTitle,
    LicenceSubtitle,
    LicenceAccept,

    LocationTitle,
    LocationSubtitle,
    LocationPerUser,
    LocationField,
    LocationTarget,
    LocationFixed,
    LocationShortcut,

    StatusChecking,
    StatusNew,
    StatusUpgrade,
    StatusInstalled,
    StatusDamaged,
    StatusNewerHeading,
    StatusNewerBody,
    StatusBusyHeading,
    StatusBusyBody,
    StatusNotAbsolute,
    StatusNotWritable,
    StatusTooDeep,

    ReadyTitle,
    ReadySubtitle,
    ReadyLocationLabel,
    ReadyKindLabel,
    ReadyKindNew,
    ReadyKindUpgrade,
    ReadyKindRepair,
    ReadyShortcutLabel,
    ReadyShortcutYes,
    ReadyShortcutNo,
    ReadyAccountLabel,
    ReadyAccountValue,
    ReadyHint,

    InstallingTitle,
    InstallingSubtitle,
    InstallingCounts,
    InstallingNote,

    FinishTitle,
    Finish,
    FinishLocation,
    FinishShortcut,
    FailedTitle,
    FailedBody,
    FailedDetails,
    FailedLog,
    LaunchFailedTitle,

    DamagedTitle,
    DamagedBody,
    CannotStartTitle,

    CancelTitle,
    CancelBody,
    CancelConfirm,
    CancelKeep,

    StepWelcome,
    StepLicence,
    StepLocation,
    StepReady,
    StepInstalling,
    StepFinish,
}

impl Key {
    /// Every key, for tests that must cover them all.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 76] = {
        use Key::*;
        [
            WindowTitle,
            Back,
            Next,
            Install,
            Close,
            Cancel,
            Browse,
            Retry,
            Launch,
            WelcomeTitle,
            WelcomeHeadline,
            WelcomeByline,
            WelcomeVerified,
            Welcome,
            WelcomeHint,
            LicenceTitle,
            LicenceSubtitle,
            LicenceAccept,
            LocationTitle,
            LocationSubtitle,
            LocationPerUser,
            LocationField,
            LocationTarget,
            LocationFixed,
            LocationShortcut,
            StatusChecking,
            StatusNew,
            StatusUpgrade,
            StatusInstalled,
            StatusDamaged,
            StatusNewerHeading,
            StatusNewerBody,
            StatusBusyHeading,
            StatusBusyBody,
            StatusNotAbsolute,
            StatusNotWritable,
            StatusTooDeep,
            ReadyTitle,
            ReadySubtitle,
            ReadyLocationLabel,
            ReadyKindLabel,
            ReadyKindNew,
            ReadyKindUpgrade,
            ReadyKindRepair,
            ReadyShortcutLabel,
            ReadyShortcutYes,
            ReadyShortcutNo,
            ReadyAccountLabel,
            ReadyAccountValue,
            ReadyHint,
            InstallingTitle,
            InstallingSubtitle,
            InstallingCounts,
            InstallingNote,
            FinishTitle,
            Finish,
            FinishLocation,
            FinishShortcut,
            FailedTitle,
            FailedBody,
            FailedDetails,
            FailedLog,
            LaunchFailedTitle,
            DamagedTitle,
            DamagedBody,
            CannotStartTitle,
            CancelTitle,
            CancelBody,
            CancelConfirm,
            CancelKeep,
            StepWelcome,
            StepLicence,
            StepLocation,
            StepReady,
            StepInstalling,
            StepFinish,
        ]
    };
}

/// The English default for a key, or `None` where a platform shows nothing.
///
/// Where a line names the publisher and the manifest names none,
/// [`anonymous_default`] supplies the version that reads correctly without.
#[allow(clippy::too_many_lines)] // One table, and splitting it would scatter it.
fn default_text(key: Key, flavour: Flavour) -> Option<&'static str> {
    let windows = flavour == Flavour::Windows;
    let either = |w: &'static str, m: &'static str| Some(if windows { w } else { m });
    let windows_only = |w: &'static str| if windows { Some(w) } else { None };

    match key {
        // The same words the installer file itself is named with, so the window
        // and the thing that was double-clicked say the same.
        Key::WindowTitle => Some("Install {name}"),
        Key::Back => either("< Back", "Go Back"),
        Key::Next => either("Next >", "Continue"),
        Key::Install => Some("Install"),
        Key::Close => Some("Close"),
        Key::Cancel => Some("Cancel"),
        Key::Browse => either("Browse…", "Choose…"),
        Key::Retry => Some("Retry"),
        Key::Launch => either("Launch {name}", "Open {name}"),

        Key::WelcomeTitle => either("Welcome to the {name} Setup Wizard", "Welcome to {name}"),
        Key::WelcomeHeadline => either("{name} {version}", "{name}"),
        Key::WelcomeByline => either("{publisher}", "Version {version} · {publisher}"),
        Key::WelcomeVerified => Some("Verified package from {publisher}"),
        Key::Welcome => Some("This will install {name} for your user account."),
        Key::WelcomeHint => windows_only("Click Next to continue, or Cancel to exit Setup."),

        Key::LicenceTitle => either("Licence Agreement", "Licence agreement"),
        Key::LicenceSubtitle => {
            windows_only("Please read the licence terms before installing {name}.")
        }
        Key::LicenceAccept => {
            either("I accept the terms of the licence agreement", "I accept the licence agreement")
        }

        Key::LocationTitle => either("Choose Install Location", "Choose where to install"),
        Key::LocationSubtitle => {
            windows_only("Choose the folder that holds your xPack applications.")
        }
        Key::LocationPerUser => either(
            "{name} is installed for your user account only. No administrator permission is \
             needed.",
            "{name} is installed for your user account only. No administrator password is \
             needed.",
        ),
        Key::LocationField => either("Applications folder:", "Applications folder"),
        Key::LocationTarget => either("{name} will be installed in:", "Installs to"),
        Key::LocationFixed => {
            Some("{name} is already installed for your account, so Setup uses the same folder.")
        }
        Key::LocationShortcut => either("Add to Start Menu", "Add to Applications"),

        Key::StatusChecking => Some("Checking this folder…"),
        Key::StatusNew => Some("{name} {new} will be installed here."),
        Key::StatusUpgrade => Some("Will upgrade {name} {old} → {new}."),
        Key::StatusInstalled => Some("{name} {new} is already installed here."),
        Key::StatusDamaged => Some("{name} {new} is damaged here and will be repaired."),
        Key::StatusNewerHeading => Some("A newer version ({v}) is already installed here."),
        Key::StatusNewerBody => Some("Setup can't replace it with {new}."),
        Key::StatusBusyHeading => Some("{name} is busy."),
        Key::StatusBusyBody => Some("It is being updated or run by another xPack operation."),
        Key::StatusNotAbsolute => Some("Enter a full folder path."),
        Key::StatusNotWritable => {
            Some("You can't install to this folder. Choose a folder in your user account.")
        }
        Key::StatusTooDeep => Some(
            "This folder is nested too deeply for {name} to start from it. Choose a shorter \
             path.",
        ),

        Key::ReadyTitle => either("Ready to Install", "Ready to install"),
        Key::ReadySubtitle => windows_only("Setup is ready to install {name} on this computer."),
        Key::ReadyLocationLabel => either("Install to", "Location"),
        Key::ReadyKindLabel => Some("Installation"),
        Key::ReadyKindNew => Some("New installation of {new}"),
        Key::ReadyKindUpgrade => Some("Upgrade from {old} to {new}"),
        Key::ReadyKindRepair => Some("Repair of {new}"),
        Key::ReadyShortcutLabel => either("Start Menu", "Applications"),
        Key::ReadyShortcutYes => either("Add a shortcut", "Adds {name} to ~/Applications"),
        Key::ReadyShortcutNo => Some("No shortcut"),
        Key::ReadyAccountLabel => Some("Account"),
        Key::ReadyAccountValue => {
            either("Current user only, no administrator rights", "Current user only")
        }
        Key::ReadyHint => either(
            "Review your choices. Click Install to continue, or Back to change them.",
            "Click Install to begin, or Go Back to change your choices.",
        ),

        Key::InstallingTitle => Some("Installing {name}"),
        Key::InstallingSubtitle => {
            windows_only("Please wait while Setup installs the application.")
        }
        Key::InstallingCounts => {
            Some("{files_done} of {files_total} files · {mb_done} of {mb_total} MB")
        }
        Key::InstallingNote => either(
            "Installation can't be stopped once it starts. It finishes in a moment.",
            "Installation can't be stopped once it starts.",
        ),

        Key::FinishTitle => either("Completing the {name} Setup Wizard", "Installation complete"),
        Key::Finish => Some("{name} is ready."),
        Key::FinishLocation => Some("Installed in {path}"),
        Key::FinishShortcut => {
            either("A shortcut was added to the Start Menu.", "Added to ~/Applications")
        }
        Key::FailedTitle => Some("Setup could not install {name}"),
        Key::FailedBody => Some(
            "{name} was not installed. If an earlier version was installed, it is still there \
             and works as before.",
        ),
        Key::FailedDetails => Some("Details"),
        Key::FailedLog => Some("A log of this attempt was written to {log}"),
        Key::LaunchFailedTitle => Some("{name} could not be started"),

        Key::DamagedTitle => Some("This installer is damaged and can't be used."),
        Key::DamagedBody => Some(
            "Its package failed the signature check, so nothing was installed. Download the \
             installer again from the publisher.",
        ),

        Key::CannotStartTitle => Some("This installer could not start."),

        Key::CancelTitle => Some("Exit Setup?"),
        Key::CancelBody => {
            Some("{name} has not been installed. You can run Setup again at any time.")
        }
        // A Windows message box cannot relabel its buttons, so the question is
        // worded to be answered "Yes" or "No".
        Key::CancelConfirm => either("Yes", "Exit Setup"),
        Key::CancelKeep => either("No", "Keep Installing"),

        // The step list beside a macOS wizard, one line per page.
        Key::StepWelcome => Some("Welcome"),
        Key::StepLicence => Some("Licence"),
        Key::StepLocation => Some("Location"),
        Key::StepReady => Some("Ready"),
        Key::StepInstalling => Some("Installing"),
        Key::StepFinish => Some("Finish"),
    }
}

/// How a line reads when the manifest names no publisher.
enum Anonymous {
    /// The line does not mention the publisher; its ordinary default applies.
    Unaffected,
    /// The line is left out entirely.
    Omitted,
    /// The line is reworded to read correctly without a publisher.
    Reworded(&'static str),
}

/// The wording for a line that names the publisher, when there is none.
fn anonymous_default(key: Key, flavour: Flavour) -> Anonymous {
    match (key, flavour) {
        (Key::WelcomeByline, Flavour::Windows) => Anonymous::Omitted,
        (Key::WelcomeByline, Flavour::Mac) => Anonymous::Reworded("Version {version}"),
        (Key::WelcomeVerified, _) => Anonymous::Reworded("Verified package"),
        _ => Anonymous::Unaffected,
    }
}

/// The wizard's text, rendered for one installation.
#[derive(Debug, Clone)]
pub struct Texts {
    flavour: Flavour,
    facts: Facts,
    overrides: BTreeMap<TextOverride, String>,
}

impl Texts {
    /// Text for this platform and these facts, with the publisher's
    /// replacements applied.
    pub fn new(flavour: Flavour, facts: Facts, overrides: BTreeMap<TextOverride, String>) -> Self {
        Self { flavour, facts, overrides }
    }

    /// A line that names nothing about the application, for the moments
    /// before there is a verified package to name: an installer that is
    /// damaged or cannot start.
    ///
    /// Empty for a key that needs facts, which is a bug in the caller rather
    /// than something to show a person.
    pub fn fixed(key: Key, flavour: Flavour) -> String {
        default_text(key, flavour)
            .filter(|template| placeholders(template).is_empty())
            .unwrap_or_default()
            .to_string()
    }

    /// The platform conventions in use.
    pub fn flavour(&self) -> Flavour {
        self.flavour
    }

    /// The facts the text is rendered from.
    pub fn facts(&self) -> &Facts {
        &self.facts
    }

    /// A line, or `None` where this platform shows nothing for it.
    pub fn get(&self, key: Key) -> Option<String> {
        self.get_with(key, &[])
    }

    /// A line with extra values for the placeholders a particular state needs,
    /// such as `{old}` on an upgrade.
    pub fn get_with(&self, key: Key, extra: &[(&str, &str)]) -> Option<String> {
        let replaced = self
            .overrides
            .iter()
            .find(|(candidate, _)| candidate.key() == key)
            .map(|(_, value)| value.as_str());

        let template = match (replaced, &self.facts.publisher) {
            (Some(value), _) => value,
            (None, None) => match anonymous_default(key, self.flavour) {
                Anonymous::Unaffected => default_text(key, self.flavour)?,
                Anonymous::Omitted => return None,
                Anonymous::Reworded(template) => template,
            },
            (None, Some(_)) => default_text(key, self.flavour)?,
        };

        let version = self.facts.version.to_string();
        let mut values: Vec<(&str, &str)> = vec![
            ("name", &self.facts.name),
            ("version", &version),
            ("new", &version),
            ("publisher", self.facts.publisher.as_deref().unwrap_or_default()),
        ];
        values.extend_from_slice(extra);
        Some(render(template, &values))
    }

    /// A line every platform shows.
    ///
    /// For keys whose default exists on both platforms; asking for one that
    /// does not is a bug in the caller, reported as an empty line rather than
    /// a crash inside a window's event handler.
    pub fn line(&self, key: Key) -> String {
        self.get(key).unwrap_or_default()
    }

    /// [`Self::line`], with extra placeholder values.
    pub fn line_with(&self, key: Key, extra: &[(&str, &str)]) -> String {
        self.get_with(key, extra).unwrap_or_default()
    }
}

/// Replaces `{placeholder}`s in a single pass.
///
/// A placeholder with no value is left as written, so a missing value is
/// visible in testing rather than silently becoming an empty string.
pub fn render(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let name_len = after.find('}').filter(|&end| is_placeholder_name(&after[..end]));
        if let Some(end) = name_len {
            let name = &after[..end];
            match values.iter().find(|(key, _)| *key == name) {
                Some((_, value)) => out.push_str(value),
                None => out.push_str(&rest[open..open + end + 2]),
            }
            rest = &after[end + 1..];
        } else {
            out.push('{');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// The placeholder names a template uses, in order.
fn placeholders(template: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        match after.find('}').filter(|&end| is_placeholder_name(&after[..end])) {
            Some(end) => {
                found.push(&after[..end]);
                rest = &after[end + 1..];
            }
            None => rest = after,
        }
    }
    found
}

fn is_placeholder_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(publisher: Option<&str>) -> Facts {
        Facts {
            name: "My Application".into(),
            version: Version::parse("2.0.0").expect("a version"),
            publisher: publisher.map(Into::into),
            description: None,
        }
    }

    fn texts(flavour: Flavour, publisher: Option<&str>) -> Texts {
        Texts::new(flavour, facts(publisher), BTreeMap::new())
    }

    #[test]
    fn placeholders_are_filled_from_the_facts() {
        let texts = texts(Flavour::Windows, Some("Example Publisher"));
        assert_eq!(texts.line(Key::WindowTitle), "Install My Application");
        assert_eq!(texts.line(Key::WelcomeHeadline), "My Application 2.0.0");
        assert_eq!(texts.line(Key::WelcomeVerified), "Verified package from Example Publisher");
    }

    #[test]
    fn the_two_platforms_follow_their_own_conventions() {
        let windows = texts(Flavour::Windows, None);
        let mac = texts(Flavour::Mac, None);
        assert_eq!(windows.line(Key::Next), "Next >");
        assert_eq!(mac.line(Key::Next), "Continue");
        assert_eq!(mac.line(Key::WindowTitle), "Install My Application");
        assert_eq!(
            windows.get(Key::WelcomeHint).as_deref(),
            Some(windows.line(Key::WelcomeHint).as_str())
        );
        assert_eq!(mac.get(Key::WelcomeHint), None, "macOS has no hint line");
    }

    #[test]
    fn extra_values_fill_state_placeholders() {
        let texts = texts(Flavour::Mac, None);
        assert_eq!(
            texts.line_with(Key::StatusUpgrade, &[("old", "1.4.2")]),
            "Will upgrade My Application 1.4.2 → 2.0.0."
        );
    }

    #[test]
    fn a_value_is_never_expanded_a_second_time() {
        // The publisher name comes from a package, and must not be able to
        // pull other values, or its own braces, into the sentence.
        let texts = texts(Flavour::Windows, Some("{version} {name}"));
        assert_eq!(texts.line(Key::WelcomeVerified), "Verified package from {version} {name}");
    }

    #[test]
    fn a_missing_publisher_changes_the_wording_rather_than_leaving_a_hole() {
        let windows = texts(Flavour::Windows, None);
        let mac = texts(Flavour::Mac, None);
        assert_eq!(windows.line(Key::WelcomeVerified), "Verified package");
        assert_eq!(windows.get(Key::WelcomeByline), None);
        assert_eq!(mac.line(Key::WelcomeByline), "Version 2.0.0");
    }

    #[test]
    fn every_line_renders_without_an_unfilled_fact() {
        // State placeholders are filled by the caller that knows them; the
        // facts must always be filled here, on either platform, with or
        // without a publisher.
        for flavour in [Flavour::Windows, Flavour::Mac] {
            for publisher in [None, Some("P")] {
                let texts = texts(flavour, publisher);
                for key in Key::ALL {
                    if let Some(line) = texts.get(key) {
                        for fact in ["{name}", "{version}", "{publisher}", "{new}"] {
                            assert!(!line.contains(fact), "{key:?} on {flavour:?}: {line}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_publisher_replacement_is_used_and_rendered() {
        let overrides =
            BTreeMap::from([(TextOverride::Welcome, "Hello from {name} {version}.".to_string())]);
        let texts = Texts::new(Flavour::Mac, facts(None), overrides);
        assert_eq!(texts.line(Key::Welcome), "Hello from My Application 2.0.0.");
        assert_eq!(texts.line(Key::Finish), "My Application is ready.", "untouched default");
    }

    #[test]
    fn a_replacement_may_only_use_facts_that_always_exist() {
        validate_override(TextOverride::Welcome, "Install {name} {version}").expect("allowed");
        // The publisher may be absent, and a state value is never available
        // on these pages.
        assert!(validate_override(TextOverride::Welcome, "By {publisher}").is_err());
        assert!(validate_override(TextOverride::Finish, "Now {old}").is_err());
    }

    #[test]
    fn a_replacement_must_be_short_printable_and_present() {
        assert!(validate_override(TextOverride::Welcome, "   ").is_err());
        assert!(validate_override(TextOverride::Welcome, "a\u{7}b").is_err());
        assert!(validate_override(TextOverride::Welcome, &"x".repeat(401)).is_err());
        validate_override(TextOverride::Welcome, "line one\nline two").expect("newline allowed");
        validate_override(TextOverride::Welcome, &"é".repeat(400)).expect("counted in chars");
    }

    #[test]
    fn fixed_lines_name_nothing_and_refuse_keys_that_would() {
        assert_eq!(
            Texts::fixed(Key::DamagedTitle, Flavour::Mac),
            "This installer is damaged and can't be used."
        );
        assert_eq!(Texts::fixed(Key::WindowTitle, Flavour::Mac), "", "needs the name");
    }

    #[test]
    fn stray_braces_are_left_alone() {
        assert_eq!(render("a {b c} {} {", &[("b", "x")]), "a {b c} {} {");
        assert_eq!(render("{unknown}", &[]), "{unknown}");
        assert_eq!(render("{{name}}", &[("name", "x")]), "{x}");
    }
}
