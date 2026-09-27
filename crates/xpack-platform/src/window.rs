//! Bringing a running application's window to the front.
//!
//! A second start of a single-instance application hands over to the copy
//! already running, and the user who clicked expects to see it. Windows is the
//! one platform where xPack does this itself: macOS does it when its bundle is
//! opened again, and Linux has no portable way at all, so there the
//! application raises itself when it reads the request.

/// Brings the application whose process is `pid` to the front, restoring it
/// if it is minimised. `true` when a window was found and raised.
///
/// Only a window belonging to that process: an application whose launch
/// program starts the real one and exits has its windows under another
/// process, and none is found. The caller carries on either way, because the
/// request it hands over lets the application raise itself.
///
/// Windows lets a process move another's window to the front only while it is
/// the foreground process itself, which a launcher the user just clicked is.
#[cfg(windows)]
pub fn bring_to_front(pid: u32) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowExW, GW_OWNER, GetWindow, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
        SW_RESTORE, SetForegroundWindow, ShowWindow,
    };

    // SAFETY, for every call in this block: each takes window handles that
    // Windows itself just returned, or null where the documentation allows it
    // (a null parent and a null "after" to walk top-level windows from the
    // start; null class and title to match any). A handle to a window that
    // closes in the meantime makes a call fail, it does not make it unsound.
    // `owner` is a live local the call writes a process id into. Nothing is
    // kept past the call. The workspace otherwise forbids unsafe code; this is
    // the only way to reach another process's window without it.
    #[allow(unsafe_code)]
    unsafe {
        let mut window = std::ptr::null_mut();
        loop {
            window =
                FindWindowExW(std::ptr::null_mut(), window, std::ptr::null(), std::ptr::null());
            if window.is_null() {
                return false;
            }
            let mut owner = 0u32;
            GetWindowThreadProcessId(window, &raw mut owner);
            // The application's main window: visible, and not a dialog or
            // tool window owned by another of its windows.
            if owner != pid
                || IsWindowVisible(window) == 0
                || !GetWindow(window, GW_OWNER).is_null()
            {
                continue;
            }
            if IsIconic(window) != 0 {
                ShowWindow(window, SW_RESTORE);
            }
            return SetForegroundWindow(window) != 0;
        }
    }
}

/// Not done here on other platforms: see the module documentation.
#[cfg(not(windows))]
pub fn bring_to_front(pid: u32) -> bool {
    let _ = pid;
    false
}
