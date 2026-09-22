//! The installer with a window.
//!
//! Builds the wizard from the verified package and hands it the installer as
//! its engine. Everything the wizard decides, and every word it shows, lives in
//! `xpack-installer-ui`; this only joins the two and turns how the run ended
//! into the exit code a script would have seen.

use std::process::ExitCode;
use std::sync::Arc;

use xpack_install::Existing;
use xpack_installer::{RootSource, exit, exit_code_for};
use xpack_installer_ui::window::{self, Shown};
use xpack_installer_ui::{
    Conclusion, Engine, Facts, FailureKind, Flavour, Key, Texts, Wizard, WizardSpec,
};

use crate::console::Args;

/// Runs the wizard, or returns `None` when no window could be shown so the
/// caller runs the console installer instead.
pub(crate) fn run(args: &Args, log: Option<std::path::PathBuf>) -> Option<ExitCode> {
    let flavour = Flavour::native();

    // An unbuilt stub has nothing to install and nobody but a developer runs
    // it; the console says so.
    crate::locate().ok()?;

    let payload = match crate::load() {
        Ok(payload) => payload,
        Err(error) => {
            // Shown instead of the wizard, so nothing unverified is ever on
            // screen. The words for a security failure are fixed; anything
            // else is the error itself.
            let (title, body) = if error.is_integrity_failure() {
                (Texts::fixed(Key::DamagedTitle, flavour), Texts::fixed(Key::DamagedBody, flavour))
            } else {
                (Texts::fixed(Key::CannotStartTitle, flavour), error.to_string())
            };
            return window::alert(&title, &body).then(|| ExitCode::from(exit_code_for(&error)));
        }
    };

    let resolved = match payload.resolve_root(args.root.as_deref()) {
        Ok(resolved) => resolved,
        Err(error) => {
            let title = Texts::fixed(Key::CannotStartTitle, flavour);
            return window::alert(&title, &error.to_string())
                .then(|| ExitCode::from(exit_code_for(&error)));
        }
    };

    // A root given on the command line was chosen already, and one that holds
    // this application — found through its entry, or already at the default —
    // stays where it is: a second copy elsewhere would fight the first over
    // the same menu entry and uninstall entry.
    let root = resolved.root;
    let root_fixed = resolved.source != RootSource::Default
        || !matches!(payload.inspect(&root), Ok(Existing::Nothing));

    let manifest = payload.manifest();
    let mut plan = payload.ui();
    if args.no_shortcut {
        plan.shortcut_default = false;
    }
    let spec = WizardSpec {
        flavour,
        facts: Facts {
            name: manifest.application.name.clone(),
            version: manifest.application.version.clone(),
            publisher: manifest.application.publisher.clone(),
            description: manifest.application.description.clone(),
        },
        plan,
        licence: payload.licence().map(str::to_owned),
        shortcut_requested: manifest.desktop.shortcut,
        root,
        root_fixed,
        log,
    };
    let icon = payload.icon().map(|icon| icon.bytes.clone());
    let engine: Arc<dyn Engine> = Arc::new(payload);

    match window::run(Wizard::new(spec), engine, icon.as_deref()) {
        Shown::NotShown => None,
        Shown::Ended(conclusion) => Some(ExitCode::from(exit_code(conclusion))),
    }
}

/// The exit code for how the wizard ended: the one the console installer
/// would have returned for the same outcome.
fn exit_code(conclusion: Conclusion) -> u8 {
    match conclusion {
        Conclusion::Installed | Conclusion::AlreadyInstalled => exit::INSTALLED,
        Conclusion::Cancelled => exit::CANCELLED,
        Conclusion::Failed(FailureKind::Integrity) => exit::INTEGRITY,
        Conclusion::Failed(FailureKind::Busy) => exit::BUSY,
        Conclusion::Failed(FailureKind::Other) => exit::FAILED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wizard_ends_with_the_code_a_script_would_have_seen() {
        assert_eq!(exit_code(Conclusion::Installed), 0);
        assert_eq!(exit_code(Conclusion::AlreadyInstalled), 0);
        assert_eq!(exit_code(Conclusion::Failed(FailureKind::Other)), 1);
        assert_eq!(exit_code(Conclusion::Failed(FailureKind::Integrity)), 3);
        assert_eq!(exit_code(Conclusion::Failed(FailureKind::Busy)), 4);
        assert_eq!(exit_code(Conclusion::Cancelled), 5);
    }
}
