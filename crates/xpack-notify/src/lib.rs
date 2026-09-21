//! The one window xPack draws.
//!
//! An update that is downloaded, verified and sitting on disk is useless to a
//! user who never learns it is there. Somebody has to say so, and the choices
//! are to make every application say it for itself or to ship something that
//! says it for them. This crate is the second answer.
//!
//! # Why it is a separate binary
//!
//! The launcher and the updater run on every start of every application and
//! must stay small, cross-compile cleanly and link nothing graphical. A dialog
//! needs the opposite: a window, a toolkit, and a platform's own idea of what
//! a message box looks like.
//!
//! Keeping them apart means the graphical dependency exists in exactly one
//! crate, which is installed only when a publisher asks for a prompt. An
//! installation that wants silent updates carries no dialog code at all, and
//! the binaries that run unconditionally are unchanged.
//!
//! # One dialog per platform, and nothing shared between them
//!
//! Windows draws a Win32 message box and macOS an `NSAlert`, each through its
//! own operating system's bindings rather than a cross-platform wrapper. The
//! alert a user sees is then the one their system draws, with its spacing,
//! typography, dark mode, localised chrome and accessibility, none of which is
//! written here or kept working here.
//!
//! Every other platform reports that it could not show a dialog, and the
//! update stays silent — exactly the behaviour of an installation that never
//! asked for a prompt.
//!
//! Each platform's bindings are declared for that platform alone, so they are
//! absent from the dependency graph, the build and the binary everywhere else.
//! Everything except the call that opens the window is ordinary
//! platform-independent code, testable on any machine, which keeps the part
//! that can only be judged on one operating system down to a single function.
//!
//! # It decides nothing
//!
//! This binary asks a question and reports the answer as an exit code. It does
//! not install, activate, defer or record anything: the caller that spawned it
//! owns the installation and every decision about it. A dialog that could
//! change an installation would be a second path to the same state, and the
//! weaker of two such paths is always the bug.

use xpack_core::{UpdateSeverity, Version};

#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(any(windows, target_os = "macos")))]
mod elsewhere;

/// What the user said, as the caller should read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Go ahead: apply the update.
    ///
    /// Also what an acknowledgement means. A prompt that offers nothing to
    /// decline still tells the caller the user has seen it, and "seen it" is
    /// what stops the same version being announced again every few hours.
    Apply,
    /// Not now.
    Later,
    /// No dialog could be shown, and the user was told nothing.
    ///
    /// Distinct from [`Self::Later`] on purpose. A user who declined has made
    /// a decision worth remembering; a dialog that never opened has not, and
    /// treating the two alike would silently turn a broken prompt into a
    /// refusal nobody made.
    NotShown,
}

impl Answer {
    /// The process exit code carrying this answer.
    ///
    /// `2` is skipped because a command line the parser rejected already exits
    /// with it, and a caller must be able to tell "the user said no" from "you
    /// invoked me wrongly".
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Apply => 0,
            Self::Later => 1,
            Self::NotShown => 3,
        }
    }
}

/// Which buttons the dialog offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Buttons {
    /// A single button. The user is being told, not asked.
    Acknowledge,
    /// Apply now, or not yet.
    ApplyOrLater,
}

/// Everything the dialog needs to know.
#[derive(Debug, Clone)]
pub struct Prompt {
    /// Application name, as a user would recognise it.
    pub application: String,
    /// Version waiting to be used.
    pub version: Version,
    /// How urgent the publisher said it is.
    pub severity: UpdateSeverity,
    /// The publisher's own title, if the manifest carried one.
    pub title: Option<String>,
    /// The publisher's own message, if the manifest carried one.
    pub message: Option<String>,
    /// The application's own icon, if the installation has one.
    ///
    /// Taken from the copy the installer keeps in the installation root rather
    /// than from inside a version directory, because a version directory is
    /// replaced by the next update and a dialog pointing into one would show a
    /// missing image the first time the application updated.
    ///
    /// Absent, each platform shows whatever it shows for a program with no
    /// icon — which is the same thing an installation whose publisher declared
    /// no icon has always shown everywhere else.
    pub icon: Option<std::path::PathBuf>,

    /// Whether the caller can actually restart the application.
    ///
    /// Without it there is nothing to accept or decline: the update is already
    /// on disk and will be used at the next start whatever the user clicks, so
    /// asking would be offering a choice that changes nothing. The dialog says
    /// so instead.
    pub can_restart: bool,
}

impl Prompt {
    /// The buttons this prompt should offer.
    ///
    /// A critical update is told, not offered: nothing older may run once it
    /// is installed, so "later" would describe an outcome the launcher will
    /// not honour.
    pub fn buttons(&self) -> Buttons {
        if !self.can_restart || self.severity == UpdateSeverity::Critical {
            Buttons::Acknowledge
        } else {
            Buttons::ApplyOrLater
        }
    }

    /// The title to show: the publisher's, or one built from what is known.
    pub fn title(&self) -> String {
        if let Some(title) = self.title.as_ref().map(|title| title.trim()).filter(|t| !t.is_empty())
        {
            return title.to_string();
        }
        match self.severity {
            UpdateSeverity::Critical => format!("{} must be updated", self.application),
            _ => format!("{} {} is ready", self.application, self.version),
        }
    }

    /// The body to show: the publisher's, or one built from what is known.
    ///
    /// The generated wording never promises more than the caller can deliver.
    /// Saying "restarting now" to a user whose application is not going to
    /// restart is how an updater stops being believed.
    pub fn message(&self) -> String {
        if let Some(message) =
            self.message.as_ref().map(|message| message.trim()).filter(|m| !m.is_empty())
        {
            return message.to_string();
        }

        let application = &self.application;
        let version = &self.version;
        match (self.can_restart, self.severity) {
            (false, UpdateSeverity::Critical) => format!(
                "{application} {version} is a required update and has been installed.\n\n\
                 It will be used the next time you start {application}, and older versions \
                 will no longer start."
            ),
            (false, _) => format!(
                "{application} {version} has been installed.\n\n\
                 It will be used the next time you start {application}."
            ),
            (true, UpdateSeverity::Critical) => format!(
                "{application} {version} is a required update and has been installed.\n\n\
                 {application} will restart to finish installing it."
            ),
            (true, _) => format!(
                "{application} {version} has been installed.\n\n\
                 Restart {application} now to use it?"
            ),
        }
    }
}

/// Shows the prompt and waits for the user.
///
/// Returns [`Answer::NotShown`] where there is no dialog to show, which is
/// every platform but Windows today, and Windows itself when the window cannot
/// be opened.
pub fn show(prompt: &Prompt) -> Answer {
    #[cfg(windows)]
    {
        windows::show(prompt)
    }
    #[cfg(target_os = "macos")]
    {
        macos::show(prompt)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        elsewhere::show(prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> Prompt {
        Prompt {
            application: "Demo".into(),
            version: Version::parse("1.2.0").unwrap(),
            severity: UpdateSeverity::Recommended,
            title: None,
            message: None,
            icon: None,
            can_restart: false,
        }
    }

    #[test]
    fn every_answer_has_its_own_exit_code() {
        // A caller branches on these, so two answers sharing a code would be
        // two outcomes it could never tell apart.
        assert_eq!(Answer::Apply.exit_code(), 0);
        assert_eq!(Answer::Later.exit_code(), 1);
        assert_eq!(Answer::NotShown.exit_code(), 3);
    }

    #[test]
    fn no_answer_uses_the_exit_code_a_rejected_command_line_takes() {
        // The parser exits with 2 before this binary runs at all.
        for answer in [Answer::Apply, Answer::Later, Answer::NotShown] {
            assert_ne!(answer.exit_code(), 2, "{answer:?} collides with a usage error");
        }
    }

    #[test]
    fn a_caller_that_cannot_restart_asks_nothing() {
        // The update is already on disk and lands at the next start whatever
        // the user clicks. Offering a choice would be offering a fiction.
        for severity in
            [UpdateSeverity::Optional, UpdateSeverity::Recommended, UpdateSeverity::Critical]
        {
            let prompt = Prompt { severity, can_restart: false, ..prompt() };
            assert_eq!(prompt.buttons(), Buttons::Acknowledge, "{severity:?}");
        }
    }

    #[test]
    fn a_critical_update_is_told_not_offered() {
        let prompt = Prompt { severity: UpdateSeverity::Critical, can_restart: true, ..prompt() };
        assert_eq!(prompt.buttons(), Buttons::Acknowledge);
    }

    #[test]
    fn an_ordinary_update_offers_the_choice_when_there_is_one_to_offer() {
        for severity in [UpdateSeverity::Optional, UpdateSeverity::Recommended] {
            let prompt = Prompt { severity, can_restart: true, ..prompt() };
            assert_eq!(prompt.buttons(), Buttons::ApplyOrLater, "{severity:?}");
        }
    }

    #[test]
    fn the_publishers_own_words_win() {
        let prompt = Prompt {
            title: Some("Security release".into()),
            message: Some("Please update.".into()),
            ..prompt()
        };
        assert_eq!(prompt.title(), "Security release");
        assert_eq!(prompt.message(), "Please update.");
    }

    #[test]
    fn blank_publisher_text_falls_back_rather_than_showing_an_empty_dialog() {
        let prompt = Prompt { title: Some("   ".into()), message: Some("\n".into()), ..prompt() };
        assert!(prompt.title().contains("Demo"), "{}", prompt.title());
        assert!(prompt.message().contains("1.2.0"), "{}", prompt.message());
    }

    #[test]
    fn the_generated_wording_names_the_application_and_the_version() {
        let prompt = prompt();
        assert!(prompt.title().contains("Demo"), "{}", prompt.title());
        assert!(prompt.title().contains("1.2.0"), "{}", prompt.title());
        assert!(prompt.message().contains("Demo"), "{}", prompt.message());
        assert!(prompt.message().contains("1.2.0"), "{}", prompt.message());
    }

    #[test]
    fn the_generated_wording_never_promises_a_restart_that_cannot_happen() {
        // Telling a user their application is about to restart, when nothing
        // is able to restart it, is how an updater stops being believed.
        for severity in
            [UpdateSeverity::Optional, UpdateSeverity::Recommended, UpdateSeverity::Critical]
        {
            let prompt = Prompt { severity, can_restart: false, ..prompt() };
            let message = prompt.message().to_lowercase();
            assert!(!message.contains("restart"), "{severity:?}: {message}");
            assert!(message.contains("next time you start"), "{severity:?}: {message}");
        }
    }

    #[test]
    fn a_critical_update_says_older_versions_will_stop_working() {
        let prompt = Prompt { severity: UpdateSeverity::Critical, ..prompt() };
        assert!(prompt.message().contains("no longer start"), "{}", prompt.message());
    }

    #[cfg(not(windows))]
    #[test]
    fn a_platform_with_no_dialog_reports_that_nothing_was_shown() {
        // Not `Later`: nobody declined, because nobody was asked.
        assert_eq!(show(&prompt()), Answer::NotShown);
    }
}
