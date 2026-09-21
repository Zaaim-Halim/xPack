//! The macOS dialog.
//!
//! `NSAlert`, through `AppKit`'s own bindings rather than a cross-platform
//! wrapper, for the same reason the Windows dialog uses Win32's: the alert a
//! user sees should be the one their operating system draws, with its own
//! spacing, typography, dark mode and accessibility, none of which is written
//! here.
//!
//! Unlike a Windows message box, `AppKit` lets the buttons be titled, so the
//! choice reads "Restart now" and "Later" instead of "Yes" and "No".
//!
//! # Why it makes itself a regular application first
//!
//! This binary has no bundle and is started from a launcher, so macOS treats
//! it as a background process: its window would open behind whatever the user
//! is looking at, which for a dialog is the same as not showing it. Setting
//! the activation policy and activating promotes the process for as long as it
//! runs, which is a second or two.

use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSAlert, NSAlertStyle, NSApplication, NSApplicationActivationPolicy, NSImage, NSModalResponse,
};
use objc2_foundation::NSString;

use crate::{Answer, Buttons, Prompt};

/// Sets the icon the Dock shows while the dialog is open.
///
/// # The only unsafe in this workspace
///
/// `setApplicationIconImage:` and `NSAlert`'s `setIcon:` are the two calls
/// this crate needs that objc2 declares unsafe; every other `AppKit` call it
/// makes is a safe function. They are unsafe because Cocoa takes the image by
/// reference and reads it later, on its own schedule, so an image being
/// mutated or freed elsewhere would be read after it stopped being valid.
///
/// # Safety
///
/// The image was created in [`show`] from a file, is owned by that stack frame
/// for longer than either call takes, and is never handed to another thread or
/// mutated after creation. `NSApplication` is main-thread-confined and the
/// caller holds a `MainThreadMarker` proving it is there.
fn set_application_icon(application: &NSApplication, icon: &NSImage) {
    #[allow(unsafe_code)]
    // SAFETY: see the function's documentation.
    unsafe {
        application.setApplicationIconImage(Some(icon));
    }
}

/// Sets the icon shown inside the alert itself.
///
/// # Safety
///
/// Identical to [`set_application_icon`]: a freshly created, uniquely owned
/// image, used on the main thread, outliving the call.
fn set_alert_icon(alert: &NSAlert, icon: &NSImage) {
    #[allow(unsafe_code)]
    // SAFETY: see the function's documentation.
    unsafe {
        alert.setIcon(Some(icon));
    }
}

/// The response `runModal` returns for the first button added.
///
/// `AppKit` numbers the buttons from this constant upwards in the order they
/// were added, so the first is the affirmative one and the second is 1001.
const FIRST_BUTTON: NSModalResponse = 1000;

/// Opens the alert and waits for the user to answer it.
pub(crate) fn show(prompt: &Prompt) -> Answer {
    // The process was started by the launcher's checker thread, so this is not
    // guaranteed to be the main thread; AppKit refuses to be driven from
    // anywhere else, and saying so beats a crash inside the framework.
    let Some(mtm) = MainThreadMarker::new() else {
        return Answer::NotShown;
    };

    let application = NSApplication::sharedApplication(mtm);
    application.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    application.activate();

    // The application's own icon, where the installation has one. Set on the
    // process as well as on the alert: the first is what the Dock shows while
    // the dialog is open, and a generic icon bouncing there is how a user
    // learns to distrust a dialog claiming to be their application.
    let icon = prompt.icon.as_ref().and_then(|path| {
        NSImage::initWithContentsOfFile(
            NSImage::alloc(),
            &NSString::from_str(&path.to_string_lossy()),
        )
    });
    if let Some(icon) = &icon {
        set_application_icon(&application, icon);
    }

    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&prompt.title()));
    alert.setInformativeText(&NSString::from_str(&prompt.message()));
    if let Some(icon) = &icon {
        set_alert_icon(&alert, icon);
    }
    alert.setAlertStyle(match prompt.severity {
        xpack_core::UpdateSeverity::Critical => NSAlertStyle::Warning,
        _ => NSAlertStyle::Informational,
    });

    // Added in order, and the order is the meaning: the first button is the
    // default one, the one return activates.
    match prompt.buttons() {
        Buttons::Acknowledge => {
            alert.addButtonWithTitle(&NSString::from_str("OK"));
        }
        Buttons::ApplyOrLater => {
            alert.addButtonWithTitle(&NSString::from_str("Restart now"));
            alert.addButtonWithTitle(&NSString::from_str("Later"));
        }
    }

    if alert.runModal() == FIRST_BUTTON { Answer::Apply } else { Answer::Later }
}
