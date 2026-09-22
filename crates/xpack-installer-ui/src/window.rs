//! Showing the wizard: the one entry point, whichever platform draws it.
//!
//! Each platform's front-end lives in its own module — `macos` for `AppKit`,
//! `windows` for Win32 —
//! draws what [`Wizard`] says and reports what the person did.
//! None of them decides anything. Where no front-end exists, nothing is shown
//! and the caller runs the installer without a window.

use std::sync::Arc;

use crate::engine::Engine;
use crate::model::{Conclusion, Wizard};

/// How showing the wizard went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// No window could be shown. Nothing was done.
    NotShown,
    /// The wizard ran and ended like this.
    Ended(Conclusion),
}

/// Shows the wizard until it ends.
///
/// `icon` is the application's icon, read from its verified package.
pub fn run(wizard: Wizard, engine: Arc<dyn Engine>, icon: Option<&[u8]>) -> Shown {
    #[cfg(target_os = "macos")]
    return crate::macos::run(wizard, engine, icon);

    #[cfg(windows)]
    return crate::windows::run(wizard, engine, icon);

    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (wizard, engine, icon);
        Shown::NotShown
    }
}

/// Shows a message in place of the wizard. `false` when it could not be.
pub fn alert(title: &str, body: &str) -> bool {
    #[cfg(target_os = "macos")]
    return crate::macos::alert(title, body);

    #[cfg(windows)]
    return crate::windows::alert(title, body);

    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (title, body);
        false
    }
}

/// Draws the wizard as it is into a TIFF image, in the light or dark
/// appearance, without showing anything.
///
/// For looking at the pages where nothing may be put on the screen. `None`
/// where there is no front-end, or off the main thread.
#[doc(hidden)]
pub fn snapshot(
    wizard: Wizard,
    engine: Arc<dyn Engine>,
    icon: Option<&[u8]>,
    dark: bool,
) -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    return crate::macos::snapshot(wizard, engine, icon, dark);

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (wizard, engine, icon, dark);
        None
    }
}
