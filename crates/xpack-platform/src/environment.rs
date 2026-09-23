//! Telling running programs that the user's environment changed.
//!
//! On Windows the user's `PATH` lives in the registry, and a program reads it
//! once, when it starts, from whoever started it. Most programs are started by
//! Explorer, which read it when the user signed in. So a directory added to
//! the `PATH` reaches no new terminal until Explorer is told to read it again,
//! and until then a command installed a moment ago is "not recognised" in
//! every window the user opens.
//!
//! Windows' answer is a broadcast: `WM_SETTINGCHANGE` with `"Environment"`,
//! sent to every top-level window. Explorer rereads the environment when it
//! gets it, and so does every other program that listens. It is what the
//! system's own Environment Variables dialog sends.

/// Tells running programs to reread the user's environment.
///
/// Returns whether the message was delivered. Not delivering it is not an
/// error: the change is already in the registry, and reaches new programs
/// after the user next signs in. Everywhere but Windows there is nothing to
/// tell, and this returns `false`.
pub fn announce_environment_change() -> bool {
    imp::announce()
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };

    /// How long each window is given to answer.
    ///
    /// A window that does not answer in time is skipped, not waited for, so a
    /// hung program elsewhere on the machine cannot hold up an installation.
    const PER_WINDOW_TIMEOUT_MS: u32 = 1000;

    pub(super) fn announce() -> bool {
        // `lParam` names what changed, as a NUL-terminated UTF-16 string.
        let area: Vec<u16> = "Environment".encode_utf16().chain(Some(0)).collect();
        let mut result: usize = 0;
        // SAFETY: every argument is valid for the duration of the call.
        // `HWND_BROADCAST` is the documented pseudo-handle for "every
        // top-level window". `area` is a live, NUL-terminated UTF-16 buffer
        // owned by this frame, which is what `WM_SETTINGCHANGE` expects in
        // `lParam`, and receivers only read it. `result` is a live `usize`
        // this call may write to. `SMTO_ABORTIFHUNG` with a timeout bounds how
        // long a window that never answers can block. Nothing is retained
        // past the call, so nothing outlives what it points to.
        #[allow(unsafe_code)]
        let sent = unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                area.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                PER_WINDOW_TIMEOUT_MS,
                &raw mut result,
            )
        };
        sent != 0
    }
}

#[cfg(not(windows))]
mod imp {
    pub(super) fn announce() -> bool {
        false
    }
}
